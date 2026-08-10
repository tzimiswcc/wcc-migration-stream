//! Migration Object Profile validation and runtime-authority boundary.
//!
//! Schema conformance and deterministic structural checks can establish that a
//! profile is well formed. They do not approve it. Runtime authority is exposed
//! only through [`ApprovedProfile`], which has no public constructor. WP-001
//! intentionally has no trusted approval registry, so validation alone can
//! never construct that authority.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io::Read;
use std::path::Path;

use jsonschema::{PatternOptions, Retrieve, Uri};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;

use crate::{KernelError, Result};

const MAXIMUM_PROFILE_IDENTIFIER_CHARS: usize = 128;
const MAXIMUM_PROFILE_VERSION_CHARS: usize = 32;
const MAXIMUM_RULE_MESSAGE_CHARS: usize = 4096;
const MAXIMUM_RULE_MESSAGE_BYTES: usize = 4096;
const MAXIMUM_PROFILE_JSON_BYTES: usize = 4 * 1024 * 1024;
const MAXIMUM_RELATIONSHIP_KEY_PAIRS: usize = 16;

/// JSON-friendly, non-authoritative facts about a validated profile.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProfileSummary {
    pub profile_id: String,
    pub version: String,
    pub status: String,
    pub entity_count: usize,
}

/// A profile that passed the source schema and the kernel's structural checks.
///
/// This type is deliberately not runtime authority. In particular, a validated
/// `REVIEWED_DRAFT` remains a draft.
#[derive(Debug)]
pub struct ValidatedProfile {
    document: Value,
    profile: ProfileDocument,
}

impl ValidatedProfile {
    /// Return immutable source JSON exactly as it was validated.
    pub const fn document(&self) -> &Value {
        &self.document
    }

    /// Return serializable identity and status facts for CLI reporting.
    pub fn summary(&self) -> ProfileSummary {
        self.profile.summary()
    }
}

/// Opaque capability proving that a canonical profile may govern runtime work.
///
/// The private field prevents callers from constructing this authority from raw
/// JSON, an EDMX-derived technical catalogue, or a non-approved profile.
#[derive(Debug)]
pub struct ApprovedProfile {
    validated: ValidatedProfile,
}

impl ApprovedProfile {
    /// Return immutable source JSON exactly as it was validated.
    pub const fn document(&self) -> &Value {
        self.validated.document()
    }

    /// Return serializable identity and status facts for CLI reporting.
    pub fn summary(&self) -> ProfileSummary {
        self.validated.summary()
    }
}

/// Validate the Draft 2020-12 schema, the profile instance, and structural links.
///
/// Formats are explicitly enabled, unknown formats are rejected, patterns use
/// the linear-time regex engine, and external schema retrieval is denied.
///
/// # Errors
///
/// Returns a typed schema, instance, semantic, JSON, or I/O error when any
/// validation stage fails.
pub fn validate_profile(
    schema_path: impl AsRef<Path>,
    profile_path: impl AsRef<Path>,
) -> Result<ValidatedProfile> {
    let schema_path = schema_path.as_ref();
    let profile_path = profile_path.as_ref();
    let schema = read_json(schema_path, KernelError::ProfileSchema)?;
    let document = read_json(profile_path, KernelError::ProfileInvalid)?;
    validate_profile_document_with_schema(&schema, &document)
}

/// Validate an in-memory profile document against the trusted local schema.
///
/// This is used by the registry when replaying immutable canonical bytes. It
/// validates structure only and never manufactures approval authority.
///
/// # Errors
///
/// Returns a typed schema, instance, or semantic error.
pub fn validate_profile_document(
    schema_path: impl AsRef<Path>,
    document: &Value,
) -> Result<ValidatedProfile> {
    let schema_path = schema_path.as_ref();
    let schema = read_json(schema_path, KernelError::ProfileSchema)?;
    validate_profile_document_with_schema(&schema, document)
}

/// Validate one already bounded profile document against one already bounded
/// exact schema value. This prevents registry registration/runtime replay from
/// reopening caller-controlled paths after their bytes have been selected.
pub(crate) fn validate_profile_document_with_schema(
    schema: &Value,
    document: &Value,
) -> Result<ValidatedProfile> {
    jsonschema::draft202012::meta::validate(schema)
        .map_err(|error| KernelError::ProfileSchema(error.to_string()))?;

    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .with_pattern_options(PatternOptions::regex())
        .with_retriever(NoExternalRetriever)
        .build(schema)
        .map_err(|error| KernelError::ProfileSchema(error.to_string()))?;

    let mut errors = validator
        .iter_errors(document)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    if !errors.is_empty() {
        errors.sort_unstable();
        return Err(KernelError::ProfileInvalid(errors.join("; ")));
    }

    let profile: ProfileDocument = serde_json::from_value(document.clone()).map_err(|error| {
        KernelError::ProfileSemantic(format!(
            "schema-valid profile could not be projected for structural checks: {error}"
        ))
    })?;
    validate_semantics(&profile)?;

    Ok(ValidatedProfile {
        document: document.clone(),
        profile,
    })
}

/// Refuse to manufacture runtime authority from a caller-supplied document.
///
/// A status label and approval-shaped strings are validation inputs, not proof
/// that the document is the canonical approved profile. A later work package
/// may construct [`ApprovedProfile`] only after binding the exact document hash
/// to a trusted approval registry. WP-001 has no such registry.
///
/// # Errors
///
/// Returns [`KernelError::GeneratedDraftAuthority`] for generated catalogues or
/// [`KernelError::ProfileNotApproved`] for every other status, including an
/// untrusted caller-supplied `APPROVED` label.
#[allow(clippy::needless_pass_by_value)] // Authority capabilities consume validated input.
pub fn require_approved(profile: ValidatedProfile) -> Result<ApprovedProfile> {
    match profile.profile.status {
        ProfileStatus::GeneratedDraft => Err(KernelError::GeneratedDraftAuthority),
        ProfileStatus::Approved => Err(KernelError::ProfileNotApproved(
            "APPROVED label is not trusted canonical approval evidence; WP-001 has no approval registry"
                .to_owned(),
        )),
        status => Err(KernelError::ProfileNotApproved(status.as_str().to_owned())),
    }
}

/// Assert runtime authority through the type system.
///
/// Supplying the reference is the assertion: raw, generated, reviewed,
/// deprecated, and retired profiles cannot satisfy this signature.
pub const fn assert_runtime_authority(_: &ApprovedProfile) {}

/// Construct runtime authority only after the registry has replayed an exact,
/// immutable approval binding for this validated document.
pub(crate) const fn trust_registry_approval(profile: ValidatedProfile) -> ApprovedProfile {
    ApprovedProfile { validated: profile }
}

fn read_json(path: &Path, invalid: fn(String) -> KernelError) -> Result<Value> {
    let bytes = read_profile_input_bounded(path, invalid)?;
    let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
    let UniqueJsonValue(value) = UniqueJsonValue::deserialize(&mut deserializer)
        .map_err(|error| invalid(format!("invalid JSON: {error}")))?;
    deserializer
        .end()
        .map_err(|error| invalid(format!("invalid JSON: {error}")))?;
    Ok(value)
}

pub(crate) fn read_profile_document_bounded(path: &Path, maximum: usize) -> Result<Value> {
    let maximum = maximum.min(MAXIMUM_PROFILE_JSON_BYTES);
    let bytes = read_profile_input_with_limit(path, maximum, KernelError::ProfileInvalid)?;
    let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
    let UniqueJsonValue(value) = UniqueJsonValue::deserialize(&mut deserializer)
        .map_err(|error| KernelError::ProfileInvalid(format!("invalid JSON: {error}")))?;
    deserializer
        .end()
        .map_err(|error| KernelError::ProfileInvalid(format!("invalid JSON: {error}")))?;
    Ok(value)
}

pub(crate) fn read_profile_schema_bounded(path: &Path, maximum: usize) -> Result<Value> {
    let maximum = maximum.min(MAXIMUM_PROFILE_JSON_BYTES);
    let bytes = read_profile_input_with_limit(path, maximum, KernelError::ProfileSchema)?;
    let mut deserializer = serde_json::Deserializer::from_slice(&bytes);
    let UniqueJsonValue(value) = UniqueJsonValue::deserialize(&mut deserializer)
        .map_err(|error| KernelError::ProfileSchema(format!("invalid JSON: {error}")))?;
    deserializer
        .end()
        .map_err(|error| KernelError::ProfileSchema(format!("invalid JSON: {error}")))?;
    Ok(value)
}

fn read_profile_input_bounded(path: &Path, invalid: fn(String) -> KernelError) -> Result<Vec<u8>> {
    read_profile_input_with_limit(path, MAXIMUM_PROFILE_JSON_BYTES, invalid)
}

fn read_profile_input_with_limit(
    path: &Path,
    maximum: usize,
    invalid: fn(String) -> KernelError,
) -> Result<Vec<u8>> {
    let named_before = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    require_profile_input_metadata(path, &named_before, maximum, invalid)?;
    let mut file = open_profile_input_nofollow(path)?;
    let opened_before = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    require_profile_input_metadata(path, &opened_before, maximum, invalid)?;
    verify_profile_input_identity(path, &file, &named_before, &opened_before, invalid)?;

    let mut bytes = Vec::with_capacity(
        usize::try_from(opened_before.len())
            .unwrap_or(maximum)
            .min(maximum),
    );
    let maximum_read = u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1);
    (&mut file)
        .take(maximum_read)
        .read_to_end(&mut bytes)
        .map_err(|error| KernelError::io(path, error))?;
    if bytes.len() > maximum {
        return Err(invalid(format!(
            "profile JSON bytes {} exceed limit {maximum}: {}",
            bytes.len(),
            path.display()
        )));
    }
    let opened_after = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    require_profile_input_metadata(path, &opened_after, maximum, invalid)?;
    if opened_after.len() != u64::try_from(bytes.len()).unwrap_or(u64::MAX) {
        return Err(invalid(format!(
            "profile JSON changed size while it was read: {}",
            path.display()
        )));
    }
    let named_after = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    require_profile_input_metadata(path, &named_after, maximum, invalid)?;
    verify_profile_input_identity(path, &file, &named_after, &opened_after, invalid)?;
    Ok(bytes)
}

fn require_profile_input_metadata(
    path: &Path,
    metadata: &Metadata,
    maximum: usize,
    invalid: fn(String) -> KernelError,
) -> Result<()> {
    if !metadata.file_type().is_file() || profile_metadata_is_reparse_point(metadata) {
        return Err(invalid(format!(
            "profile JSON must be a regular non-link file: {}",
            path.display()
        )));
    }
    if metadata.len() > u64::try_from(maximum).unwrap_or(u64::MAX) {
        return Err(invalid(format!(
            "profile JSON bytes {} exceed limit {maximum}: {}",
            metadata.len(),
            path.display()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn profile_metadata_is_reparse_point(metadata: &Metadata) -> bool {
    crate::durability::is_reparse_point(metadata)
}

#[cfg(not(windows))]
const fn profile_metadata_is_reparse_point(_metadata: &Metadata) -> bool {
    false
}

fn verify_profile_input_identity(
    path: &Path,
    file: &File,
    named: &Metadata,
    opened: &Metadata,
    invalid: fn(String) -> KernelError,
) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if named.dev() != opened.dev() || named.ino() != opened.ino() {
            return Err(invalid(format!(
                "profile JSON path no longer names the verified file: {}",
                path.display()
            )));
        }
    }
    #[cfg(windows)]
    if !crate::durability::path_names_open_file(path, file)
        .map_err(|error| KernelError::io(path, error))?
    {
        return Err(invalid(format!(
            "profile JSON path no longer names the verified file: {}",
            path.display()
        )));
    }
    #[cfg(not(any(unix, windows)))]
    if named.len() != opened.len() {
        return Err(invalid(format!(
            "profile JSON path changed while it was read: {}",
            path.display()
        )));
    }
    #[cfg(windows)]
    let _ = (named, opened);
    #[cfg(unix)]
    let _ = file;
    Ok(())
}

#[cfg(unix)]
fn open_profile_input_nofollow(path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags};
    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|error| KernelError::io(path, error.into()))?;
    Ok(File::from(descriptor))
}

#[cfg(windows)]
fn open_profile_input_nofollow(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| KernelError::io(path, error))
}

#[cfg(not(any(unix, windows)))]
fn open_profile_input_nofollow(path: &Path) -> Result<File> {
    File::open(path).map_err(|error| KernelError::io(path, error))
}

struct UniqueJsonValue(Value);

impl<'de> Deserialize<'de> for UniqueJsonValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct UniqueJsonVisitor;

        impl<'de> Visitor<'de> for UniqueJsonVisitor {
            type Value = UniqueJsonValue;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value without duplicate object keys")
            }

            fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::Bool(value)))
            }

            fn visit_i64<E>(self, value: i64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::Number(value.into())))
            }

            fn visit_u64<E>(self, value: u64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::Number(value.into())))
            }

            fn visit_f64<E>(self, value: f64) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                serde_json::Number::from_f64(value)
                    .map(Value::Number)
                    .map(UniqueJsonValue)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }

            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::String(value.to_owned())))
            }

            fn visit_string<E>(self, value: String) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::String(value)))
            }

            fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::Null))
            }

            fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueJsonValue(Value::Null))
            }

            fn visit_some<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                UniqueJsonValue::deserialize(deserializer)
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while let Some(UniqueJsonValue(value)) = sequence.next_element()? {
                    values.push(value);
                }
                Ok(UniqueJsonValue(Value::Array(values)))
            }

            fn visit_map<A>(self, mut map: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut values = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if values.contains_key(&key) {
                        return Err(de::Error::custom(format!(
                            "duplicate JSON object key {key:?}"
                        )));
                    }
                    let UniqueJsonValue(value) = map.next_value()?;
                    values.insert(key, value);
                }
                Ok(UniqueJsonValue(Value::Object(values)))
            }
        }

        deserializer.deserialize_any(UniqueJsonVisitor)
    }
}

fn validate_semantics(profile: &ProfileDocument) -> Result<()> {
    require_nonblank_character_bound(
        "profileId",
        &profile.profile_id,
        MAXIMUM_PROFILE_IDENTIFIER_CHARS,
    )?;
    require_nonblank_character_bound(
        "profile version",
        &profile.version,
        MAXIMUM_PROFILE_VERSION_CHARS,
    )?;
    let mut entity_fields = HashMap::with_capacity(profile.entities.len());
    let mut entity_required_fields = HashMap::with_capacity(profile.entities.len());
    let mut entity_keys = HashMap::with_capacity(profile.entities.len());
    let mut entity_ids = HashSet::with_capacity(profile.entities.len());

    for entity in &profile.entities {
        require_nonblank_character_bound("entityId", &entity.id, MAXIMUM_PROFILE_IDENTIFIER_CHARS)?;
        if !entity_ids.insert(entity.id.as_str()) {
            return semantic_error(format!("duplicate entityId {:?}", entity.id));
        }
        if entity.fields.is_empty() {
            return semantic_error(format!(
                "entity {:?} must contain at least one field",
                entity.id
            ));
        }
        if entity.key.is_empty() {
            return semantic_error(format!(
                "entity {:?} must contain at least one key field",
                entity.id
            ));
        }

        let mut fields = HashSet::with_capacity(entity.fields.len());
        let mut required_fields = HashSet::with_capacity(entity.fields.len());
        for field in &entity.fields {
            require_nonblank_character_bound(
                &format!("entity {:?} fieldId", entity.id),
                &field.field_id,
                MAXIMUM_PROFILE_IDENTIFIER_CHARS,
            )?;
            if !fields.insert(field.field_id.as_str()) {
                return semantic_error(format!(
                    "entity {:?} contains duplicate fieldId {:?}",
                    entity.id, field.field_id
                ));
            }
            if field.required {
                required_fields.insert(field.field_id.as_str());
            }
        }

        let mut keys = HashSet::with_capacity(entity.key.len());
        for key in &entity.key {
            if !fields.contains(key.as_str()) {
                return semantic_error(format!(
                    "entity {:?} key {:?} does not resolve to one of its fields",
                    entity.id, key
                ));
            }
            if !keys.insert(key.as_str()) {
                return semantic_error(format!(
                    "entity {:?} contains duplicate key field {:?}",
                    entity.id, key
                ));
            }
        }

        entity_fields.insert(entity.id.as_str(), fields);
        entity_required_fields.insert(entity.id.as_str(), required_fields);
        entity_keys.insert(
            entity.id.as_str(),
            entity.key.iter().map(String::as_str).collect::<Vec<_>>(),
        );
    }

    let root_count = profile.entities.iter().filter(|entity| entity.root).count();
    if root_count != 1 {
        return semantic_error(format!(
            "profile must contain exactly one root entity; found {root_count}"
        ));
    }

    validate_relationships(
        profile,
        &entity_fields,
        &entity_required_fields,
        &entity_keys,
    )?;
    validate_rules(profile, &entity_fields)?;
    validate_adapters(profile)?;

    if profile.status == ProfileStatus::Approved {
        require_complete_approval(profile)?;
    }
    // Approval-shaped data on any other status never grants authority. The
    // `require_approved` still refuses authority because this validation does
    // not authenticate a canonical profile or its approval evidence.

    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_relationships(
    profile: &ProfileDocument,
    entity_fields: &HashMap<&str, HashSet<&str>>,
    entity_required_fields: &HashMap<&str, HashSet<&str>>,
    entity_keys: &HashMap<&str, Vec<&str>>,
) -> Result<()> {
    let mut relationships = HashSet::with_capacity(profile.relationships.len());
    let mut relationship_ids = HashSet::with_capacity(profile.relationships.len());
    for relationship in &profile.relationships {
        if !entity_fields.contains_key(relationship.from.as_str()) {
            return semantic_error(format!(
                "relationship source {:?} does not resolve to an entity",
                relationship.from
            ));
        }
        if !entity_fields.contains_key(relationship.to.as_str()) {
            return semantic_error(format!(
                "relationship target {:?} does not resolve to an entity",
                relationship.to
            ));
        }

        // Legacy profiles may omit relationshipId/keyPairs. Preserve their
        // historical tuple-uniqueness contract without inferring a keyed join.
        let identity = (
            relationship.from.as_str(),
            relationship.to.as_str(),
            relationship.cardinality.as_str(),
            relationship.technical_navigation.as_deref(),
        );
        if !relationships.insert(identity) {
            return semantic_error(format!(
                "duplicate relationship tuple ({:?}, {:?}, {:?}, {:?})",
                relationship.from,
                relationship.to,
                relationship.cardinality,
                relationship.technical_navigation.as_deref()
            ));
        }

        let (relationship_id, key_pairs) = match (&relationship.id, &relationship.key_pairs) {
            (None, None) => continue,
            (Some(relationship_id), Some(key_pairs)) => (relationship_id, key_pairs),
            _ => {
                return semantic_error(
                    "explicit relationship semantics require both relationshipId and keyPairs",
                );
            }
        };
        require_nonblank_character_bound(
            "relationshipId",
            relationship_id,
            MAXIMUM_PROFILE_IDENTIFIER_CHARS,
        )?;
        if !relationship_ids.insert(relationship_id.as_str()) {
            return semantic_error(format!("duplicate relationshipId {relationship_id:?}"));
        }
        if !matches!(relationship.cardinality.as_str(), "0..*" | "1..*") {
            return semantic_error(format!(
                "keyed relationship {:?} has unsupported cardinality {:?}; bounded validation supports only 0..* and 1..*",
                relationship_id, relationship.cardinality
            ));
        }
        if key_pairs.is_empty() || key_pairs.len() > MAXIMUM_RELATIONSHIP_KEY_PAIRS {
            return semantic_error(format!(
                "relationship {relationship_id:?} keyPairs must contain 1..={MAXIMUM_RELATIONSHIP_KEY_PAIRS} pairs"
            ));
        }
        let from_fields = &entity_fields[relationship.from.as_str()];
        let to_fields = &entity_fields[relationship.to.as_str()];
        let required_from = &entity_required_fields[relationship.from.as_str()];
        let required_to = &entity_required_fields[relationship.to.as_str()];
        let mut seen_from = HashSet::with_capacity(key_pairs.len());
        let mut seen_to = HashSet::with_capacity(key_pairs.len());
        for pair in key_pairs {
            if !from_fields.contains(pair.from_field_id.as_str()) {
                return semantic_error(format!(
                    "relationship {:?} fromFieldId {:?} does not resolve on entity {:?}",
                    relationship_id, pair.from_field_id, relationship.from
                ));
            }
            if !to_fields.contains(pair.to_field_id.as_str()) {
                return semantic_error(format!(
                    "relationship {:?} toFieldId {:?} does not resolve on entity {:?}",
                    relationship_id, pair.to_field_id, relationship.to
                ));
            }
            if !required_from.contains(pair.from_field_id.as_str())
                || !required_to.contains(pair.to_field_id.as_str())
            {
                return semantic_error(format!(
                    "relationship {relationship_id:?} key fields must be required and non-nullable"
                ));
            }
            if !seen_from.insert(pair.from_field_id.as_str())
                || !seen_to.insert(pair.to_field_id.as_str())
            {
                return semantic_error(format!(
                    "relationship {relationship_id:?} keyPairs repeat an endpoint field"
                ));
            }
        }
        let declared_parent_key = key_pairs
            .iter()
            .map(|pair| pair.from_field_id.as_str())
            .collect::<Vec<_>>();
        if entity_keys[relationship.from.as_str()] != declared_parent_key {
            return semantic_error(format!(
                "relationship {:?} fromFieldId sequence must exactly match entity {:?} key",
                relationship_id, relationship.from
            ));
        }
    }
    Ok(())
}

fn validate_rules(
    profile: &ProfileDocument,
    entity_fields: &HashMap<&str, HashSet<&str>>,
) -> Result<()> {
    let mut rule_ids = HashSet::with_capacity(profile.rules.len());
    let mut reference_table_versions = HashMap::<(&str, &str), &str>::new();
    for rule in &profile.rules {
        require_nonblank_character_bound("ruleId", &rule.id, MAXIMUM_PROFILE_IDENTIFIER_CHARS)?;
        if !rule_ids.insert(rule.id.as_str()) {
            return semantic_error(format!("duplicate ruleId {:?}", rule.id));
        }
        if rule.message.trim().is_empty()
            || rule.message.chars().count() > MAXIMUM_RULE_MESSAGE_CHARS
            || rule.message.len() > MAXIMUM_RULE_MESSAGE_BYTES
        {
            return semantic_error(format!(
                "rule {:?} message must contain 1..={MAXIMUM_RULE_MESSAGE_CHARS} characters and no more than {MAXIMUM_RULE_MESSAGE_BYTES} UTF-8 bytes",
                rule.id
            ));
        }

        let Some((entity_id, field_id)) = structured_rule_target(&rule.target) else {
            return semantic_error(format!(
                "rule {:?} target {:?} must have exactly Entity.field structure",
                rule.id, rule.target
            ));
        };
        let Some(fields) = entity_fields.get(entity_id) else {
            return semantic_error(format!(
                "rule {:?} target entity {:?} does not resolve",
                rule.id, entity_id
            ));
        };
        if !fields.contains(field_id) {
            return semantic_error(format!(
                "rule {:?} target field {:?}.{:?} does not resolve",
                rule.id, entity_id, field_id
            ));
        }
        validate_rule_parameters(rule, entity_fields)?;
        if rule.kind == "reference_lookup" {
            let parameters = rule
                .parameters
                .as_object()
                .expect("reference_lookup parameters were validated above");
            let table_id = parameters["tableId"]
                .as_str()
                .expect("reference_lookup tableId was validated above");
            let table_version = parameters["tableVersion"]
                .as_str()
                .expect("reference_lookup tableVersion was validated above");
            if let Some(existing_version) =
                reference_table_versions.insert((entity_id, table_id), table_version)
                && existing_version != table_version
            {
                return semantic_error(format!(
                    "reference_lookup rules for {entity_id}.{table_id} declare conflicting table versions {existing_version:?} and {table_version:?}"
                ));
            }
        }
    }
    Ok(())
}

fn require_nonblank_character_bound(label: &str, value: &str, maximum: usize) -> Result<()> {
    let characters = value.chars().count();
    if value.trim().is_empty() || characters > maximum {
        return semantic_error(format!("{label} must contain 1..={maximum} characters"));
    }
    Ok(())
}

#[allow(clippy::too_many_lines)] // A single closed matrix keeps every allow-listed rule kind auditable.
fn validate_rule_parameters(
    rule: &Rule,
    entity_fields: &HashMap<&str, HashSet<&str>>,
) -> Result<()> {
    let parameters = rule.parameters.as_object().ok_or_else(|| {
        KernelError::ProfileSemantic(format!("rule {:?} parameters must be an object", rule.id))
    })?;
    match rule.kind.as_str() {
        "required" => require_parameter_keys(rule, parameters, &[]),
        "max_length" => {
            require_parameter_keys(rule, parameters, &["maximum"])?;
            let maximum = parameters["maximum"].as_u64().ok_or_else(|| {
                KernelError::ProfileSemantic(format!(
                    "rule {:?} maximum must be a positive integer",
                    rule.id
                ))
            })?;
            if maximum == 0 || maximum > 1_000_000 {
                return semantic_error(format!(
                    "rule {:?} maximum must be in 1..=1000000",
                    rule.id
                ));
            }
            Ok(())
        }
        "allowed_values" => {
            require_parameter_keys(rule, parameters, &["values"])?;
            let values = parameters["values"].as_array().ok_or_else(|| {
                KernelError::ProfileSemantic(format!("rule {:?} values must be an array", rule.id))
            })?;
            if values.is_empty() || values.len() > 10_000 {
                return semantic_error(format!(
                    "rule {:?} values must contain 1..=10000 strings",
                    rule.id
                ));
            }
            let mut unique = HashSet::with_capacity(values.len());
            for value in values {
                let value = value.as_str().ok_or_else(|| {
                    KernelError::ProfileSemantic(format!(
                        "rule {:?} allowed values must be strings",
                        rule.id
                    ))
                })?;
                if value.len() > 4096 || !unique.insert(value) {
                    return semantic_error(format!(
                        "rule {:?} allowed values must be unique and at most 4096 bytes",
                        rule.id
                    ));
                }
            }
            Ok(())
        }
        "pattern" => {
            require_parameter_keys(rule, parameters, &["pattern"])?;
            let pattern = parameters["pattern"].as_str().ok_or_else(|| {
                KernelError::ProfileSemantic(format!("rule {:?} pattern must be a string", rule.id))
            })?;
            if pattern.is_empty() || pattern.len() > 4096 {
                return semantic_error(format!(
                    "rule {:?} pattern must contain 1..=4096 bytes",
                    rule.id
                ));
            }
            regex::Regex::new(pattern).map_err(|error| {
                KernelError::ProfileSemantic(format!(
                    "rule {:?} pattern is invalid: {error}",
                    rule.id
                ))
            })?;
            Ok(())
        }
        "unique" => {
            require_parameter_keys(rule, parameters, &["scopeFields"])?;
            let fields = parameters["scopeFields"].as_array().ok_or_else(|| {
                KernelError::ProfileSemantic(format!(
                    "rule {:?} scopeFields must be an array",
                    rule.id
                ))
            })?;
            if fields.is_empty() || fields.len() > 64 {
                return semantic_error(format!(
                    "rule {:?} scopeFields must contain 1..=64 fields",
                    rule.id
                ));
            }
            let (entity_id, target_field) = structured_rule_target(&rule.target)
                .expect("rule target was validated before parameters");
            let allowed = &entity_fields[entity_id];
            let mut unique = HashSet::with_capacity(fields.len());
            for field in fields {
                let field = field.as_str().ok_or_else(|| {
                    KernelError::ProfileSemantic(format!(
                        "rule {:?} scopeFields must be strings",
                        rule.id
                    ))
                })?;
                if !allowed.contains(field) || !unique.insert(field) {
                    return semantic_error(format!(
                        "rule {:?} scope field {:?} is unknown or repeated",
                        rule.id, field
                    ));
                }
            }
            if !unique.contains(target_field) {
                return semantic_error(format!(
                    "rule {:?} unique scope must include its target field {:?}",
                    rule.id, target_field
                ));
            }
            Ok(())
        }
        "conditional_required" => {
            require_parameter_keys(rule, parameters, &["whenField", "equals"])?;
            let when_field = parameters["whenField"].as_str().ok_or_else(|| {
                KernelError::ProfileSemantic(format!(
                    "rule {:?} whenField must be a string",
                    rule.id
                ))
            })?;
            let equals = parameters["equals"].as_str().ok_or_else(|| {
                KernelError::ProfileSemantic(format!("rule {:?} equals must be a string", rule.id))
            })?;
            if equals.len() > 4096 {
                return semantic_error(format!("rule {:?} equals exceeds 4096 bytes", rule.id));
            }
            let Some((entity, field)) = structured_rule_target(when_field) else {
                return semantic_error(format!(
                    "rule {:?} whenField must have Entity.field structure",
                    rule.id
                ));
            };
            let (target_entity, _) = structured_rule_target(&rule.target)
                .expect("rule target was validated before parameters");
            if entity != target_entity {
                return semantic_error(format!(
                    "rule {:?} whenField must target the same entity as the rule",
                    rule.id
                ));
            }
            if entity_fields
                .get(entity)
                .is_none_or(|fields| !fields.contains(field))
            {
                return semantic_error(format!(
                    "rule {:?} whenField {:?} does not resolve",
                    rule.id, when_field
                ));
            }
            Ok(())
        }
        "reference_lookup" => {
            require_parameter_keys(rule, parameters, &["tableId", "tableVersion"])?;
            for key in ["tableId", "tableVersion"] {
                let value = parameters[key].as_str().ok_or_else(|| {
                    KernelError::ProfileSemantic(format!(
                        "rule {:?} {key} must be a string",
                        rule.id
                    ))
                })?;
                if value.is_empty() || value.len() > 128 {
                    return semantic_error(format!(
                        "rule {:?} {key} must contain 1..=128 bytes",
                        rule.id
                    ));
                }
            }
            Ok(())
        }
        other => semantic_error(format!("rule {:?} has unsupported kind {other:?}", rule.id)),
    }
}

fn require_parameter_keys(
    rule: &Rule,
    parameters: &serde_json::Map<String, Value>,
    expected: &[&str],
) -> Result<()> {
    let actual = parameters
        .keys()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let expected = expected.iter().copied().collect::<HashSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        semantic_error(format!(
            "rule {:?} parameter keys must be exactly {:?}",
            rule.id, expected
        ))
    }
}

fn structured_rule_target(target: &str) -> Option<(&str, &str)> {
    let mut parts = target.split('.');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(entity_id), Some(field_id), None)
            if !entity_id.trim().is_empty() && !field_id.trim().is_empty() =>
        {
            Some((entity_id, field_id))
        }
        _ => None,
    }
}

fn validate_adapters(profile: &ProfileDocument) -> Result<()> {
    let mut adapter_ids = HashSet::with_capacity(profile.publication_adapters.len());
    for adapter in &profile.publication_adapters {
        if adapter.adapter_id.trim().is_empty() {
            return semantic_error("publication adapterId must not be empty");
        }
        if !adapter_ids.insert(adapter.adapter_id.as_str()) {
            return semantic_error(format!(
                "duplicate publication adapterId {:?}",
                adapter.adapter_id
            ));
        }
        if adapter.status == AdapterStatus::Active
            && adapter
                .version
                .as_deref()
                .is_none_or(|version| version.trim().is_empty())
        {
            return semantic_error(format!(
                "active publication adapter {:?} must have a nonempty version",
                adapter.adapter_id
            ));
        }
    }
    Ok(())
}

fn require_complete_approval(profile: &ProfileDocument) -> Result<()> {
    let Some(approval) = &profile.approval else {
        return semantic_error("APPROVED profile requires approval evidence");
    };
    for (name, value) in [
        ("approvedBy", approval.approved_by.as_deref()),
        ("approvedAt", approval.approved_at.as_deref()),
        ("evidence", approval.evidence.as_deref()),
    ] {
        if value.is_none_or(|text| text.trim().is_empty()) {
            return semantic_error(format!(
                "APPROVED profile requires nonempty approval.{name}"
            ));
        }
    }
    Ok(())
}

fn semantic_error<T>(message: impl Into<String>) -> Result<T> {
    Err(KernelError::ProfileSemantic(message.into()))
}

#[derive(Debug)]
struct NoExternalRetriever;

impl Retrieve for NoExternalRetriever {
    fn retrieve(
        &self,
        uri: &Uri<String>,
    ) -> std::result::Result<Value, Box<dyn Error + Send + Sync>> {
        Err(Box::new(ExternalReferenceDisabled(uri.as_str().to_owned())))
    }
}

#[derive(Debug)]
struct ExternalReferenceDisabled(String);

impl fmt::Display for ExternalReferenceDisabled {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "external schema reference resolution is disabled: {}",
            self.0
        )
    }
}

impl Error for ExternalReferenceDisabled {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileDocument {
    profile_id: String,
    version: String,
    status: ProfileStatus,
    entities: Vec<Entity>,
    relationships: Vec<Relationship>,
    rules: Vec<Rule>,
    publication_adapters: Vec<PublicationAdapter>,
    approval: Option<Approval>,
}

impl ProfileDocument {
    fn summary(&self) -> ProfileSummary {
        ProfileSummary {
            profile_id: self.profile_id.clone(),
            version: self.version.clone(),
            status: self.status.as_str().to_owned(),
            entity_count: self.entities.len(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ProfileStatus {
    GeneratedDraft,
    ReviewedDraft,
    Approved,
    Deprecated,
    Retired,
}

impl ProfileStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::GeneratedDraft => "GENERATED_DRAFT",
            Self::ReviewedDraft => "REVIEWED_DRAFT",
            Self::Approved => "APPROVED",
            Self::Deprecated => "DEPRECATED",
            Self::Retired => "RETIRED",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entity {
    #[serde(rename = "entityId")]
    id: String,
    root: bool,
    key: Vec<String>,
    fields: Vec<Field>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Field {
    field_id: String,
    required: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Relationship {
    #[serde(rename = "relationshipId")]
    id: Option<String>,
    from: String,
    to: String,
    cardinality: String,
    technical_navigation: Option<String>,
    key_pairs: Option<Vec<RelationshipKeyPair>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelationshipKeyPair {
    from_field_id: String,
    to_field_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rule {
    #[serde(rename = "ruleId")]
    id: String,
    kind: String,
    target: String,
    #[serde(default = "empty_rule_parameters")]
    parameters: Value,
    message: String,
}

fn empty_rule_parameters() -> Value {
    Value::Object(serde_json::Map::new())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublicationAdapter {
    adapter_id: String,
    version: Option<String>,
    status: AdapterStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum AdapterStatus {
    Active,
    Future,
    Deprecated,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Approval {
    approved_by: Option<String>,
    approved_at: Option<String>,
    evidence: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::{fs, io::Write};

    use serde_json::{Value, json};
    use tempfile::{NamedTempFile, tempdir};

    use super::{
        MAXIMUM_PROFILE_IDENTIFIER_CHARS, MAXIMUM_PROFILE_JSON_BYTES,
        MAXIMUM_PROFILE_VERSION_CHARS, MAXIMUM_RULE_MESSAGE_CHARS, ProfileDocument, ProfileSummary,
        ValidatedProfile, require_approved, validate_profile, validate_semantics,
    };
    use crate::KernelError;

    fn manifest_path(path: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
    }

    fn schema_path() -> PathBuf {
        manifest_path("schemas/migration-object-profile.schema.json")
    }

    fn fixture_path() -> PathBuf {
        manifest_path("profiles/synthetic.customer-account.readiness/profile.example.json")
    }

    fn sales_document_fixture_path() -> PathBuf {
        manifest_path(
            "object-packs/synthetic.sales-document.readiness/1.0.0/synthetic-profile.reviewed.json",
        )
    }

    fn read_value(path: &Path) -> Value {
        let bytes = fs::read(path).expect("test JSON should be readable");
        serde_json::from_slice(&bytes).expect("test JSON should parse")
    }

    fn write_value(value: &Value) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("temporary file should be created");
        serde_json::to_writer(file.as_file_mut(), value).expect("test JSON should serialize");
        file
    }

    #[cfg(unix)]
    fn symlink_file(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).expect("create profile-input symlink");
    }

    #[cfg(windows)]
    fn symlink_file(target: &Path, link: &Path) {
        std::os::windows::fs::symlink_file(target, link)
            .expect("create profile-input reparse point");
    }

    fn validate_value(value: &Value) -> crate::Result<ValidatedProfile> {
        let file = write_value(value);
        validate_profile(schema_path(), file.path())
    }

    fn assert_semantic_invalid(value: &Value, expected: &str) {
        let error = validate_value(value).expect_err("profile should be semantically invalid");
        match error {
            KernelError::ProfileSemantic(detail) => assert!(
                detail.contains(expected),
                "expected {expected:?} in semantic error {detail:?}"
            ),
            other => panic!("expected semantic error, found {other:?}"),
        }
    }

    fn assert_runtime_semantic_invalid(value: &Value, expected: &str) {
        let profile: ProfileDocument = serde_json::from_value(value.clone())
            .expect("test profile should deserialize for semantic-only validation");
        let error = validate_semantics(&profile)
            .expect_err("profile should fail the runtime semantic boundary");
        match error {
            KernelError::ProfileSemantic(detail) => assert!(
                detail.contains(expected),
                "expected {expected:?} in semantic error {detail:?}"
            ),
            other => panic!("expected semantic error, found {other:?}"),
        }
    }

    fn rename_first_entity(profile: &mut Value, replacement: &str) {
        let original = profile["entities"][0]["entityId"]
            .as_str()
            .expect("first entity ID")
            .to_owned();
        profile["entities"][0]["entityId"] = json!(replacement);
        for relationship in profile["relationships"]
            .as_array_mut()
            .expect("relationships array")
        {
            for endpoint in ["from", "to"] {
                if relationship[endpoint].as_str() == Some(original.as_str()) {
                    relationship[endpoint] = json!(replacement);
                }
            }
        }
        for rule in profile["rules"].as_array_mut().expect("rules array") {
            let replacement_target = rule["target"]
                .as_str()
                .and_then(|target| target.strip_prefix(&format!("{original}.")))
                .map(|field| format!("{replacement}.{field}"));
            if let Some(replacement_target) = replacement_target {
                rule["target"] = json!(replacement_target);
            }
        }
    }

    fn synthetic_approved_profile() -> Value {
        let mut profile = read_value(&fixture_path());
        profile["profileId"] = json!("test.synthetic-profile");
        profile["title"] = json!("Synthetic authority-boundary test profile");
        profile["status"] = json!("APPROVED");
        profile["approval"] = json!({
            "approvedBy": "test-human-reviewer",
            "approvedAt": "2026-07-21T12:00:00Z",
            "evidence": "test-only synthetic approval evidence"
        });
        profile
    }

    #[test]
    fn copied_reviewed_fixture_validates_but_is_not_authority() {
        let validated =
            validate_profile(schema_path(), fixture_path()).expect("fixture should validate");
        assert_eq!(
            validated.summary(),
            ProfileSummary {
                profile_id: "synthetic.customer-account.readiness".to_owned(),
                version: "0.1.0".to_owned(),
                status: "REVIEWED_DRAFT".to_owned(),
                entity_count: 3,
            }
        );
        assert_eq!(
            validated.document()["status"],
            Value::String("REVIEWED_DRAFT".to_owned())
        );
        assert!(
            validated.document()["rules"][0].get("parameters").is_none(),
            "validation must not rewrite canonical profile bytes when defaulting rule parameters"
        );

        let error = require_approved(validated).expect_err("reviewed draft must not be authority");
        assert!(
            matches!(error, KernelError::ProfileNotApproved(status) if status == "REVIEWED_DRAFT")
        );
    }

    #[test]
    fn approval_label_and_strings_cannot_forge_runtime_authority() {
        let validated =
            validate_value(&synthetic_approved_profile()).expect("profile should validate");
        let error = require_approved(validated)
            .expect_err("caller-supplied approval strings must not grant authority");
        assert!(
            matches!(error, KernelError::ProfileNotApproved(detail) if detail.contains("no approval registry"))
        );
    }

    #[test]
    fn generated_draft_cannot_cross_authority_boundary() {
        let mut profile = read_value(&fixture_path());
        profile["status"] = json!("GENERATED_DRAFT");
        let validated =
            validate_value(&profile).expect("generated draft should be structurally valid");
        assert!(matches!(
            require_approved(validated),
            Err(KernelError::GeneratedDraftAuthority)
        ));
    }

    #[test]
    fn format_validation_is_explicitly_enforced() {
        let mut profile = read_value(&fixture_path());
        profile["technicalSources"][0]["extractedAt"] = json!("not-a-date-time");
        assert!(matches!(
            validate_value(&profile),
            Err(KernelError::ProfileInvalid(_))
        ));
    }

    #[test]
    fn schema_meta_validity_is_checked_before_instances() {
        let mut schema = read_value(&schema_path());
        schema["type"] = json!("not-a-json-schema-type");
        let schema_file = write_value(&schema);
        assert!(matches!(
            validate_profile(schema_file.path(), fixture_path()),
            Err(KernelError::ProfileSchema(_))
        ));
    }

    #[test]
    fn external_schema_resolution_is_denied() {
        let mut schema = read_value(&schema_path());
        schema["properties"]["title"] =
            json!({"$ref": "https://example.invalid/external-schema.json"});
        let schema_file = write_value(&schema);
        let error = validate_profile(schema_file.path(), fixture_path())
            .expect_err("external schema reference must not be retrieved");
        assert!(matches!(error, KernelError::ProfileSchema(_)));
    }

    #[test]
    fn duplicate_json_keys_are_rejected_before_profile_validation() {
        let source = fs::read_to_string(fixture_path()).expect("fixture text");
        let duplicate = source.replacen(
            r#""status": "REVIEWED_DRAFT""#,
            r#""status": "REVIEWED_DRAFT", "status": "APPROVED""#,
            1,
        );
        let mut file = NamedTempFile::new().expect("temporary file");
        file.write_all(duplicate.as_bytes())
            .expect("write raw JSON");
        let error = validate_profile(schema_path(), file.path())
            .expect_err("duplicate status keys must be rejected");
        assert!(
            matches!(error, KernelError::ProfileInvalid(detail) if detail.contains("duplicate JSON object key \"status\""))
        );
    }

    #[test]
    fn profile_and_schema_inputs_are_bounded_before_allocation() {
        let oversized_profile = NamedTempFile::new().expect("temporary oversized profile");
        oversized_profile
            .as_file()
            .set_len((MAXIMUM_PROFILE_JSON_BYTES + 1) as u64)
            .expect("size oversized profile");
        assert_eq!(
            validate_profile(schema_path(), oversized_profile.path())
                .unwrap_err()
                .code(),
            "PROFILE_INVALID"
        );

        let oversized_schema = NamedTempFile::new().expect("temporary oversized schema");
        oversized_schema
            .as_file()
            .set_len((MAXIMUM_PROFILE_JSON_BYTES + 1) as u64)
            .expect("size oversized schema");
        assert_eq!(
            validate_profile(oversized_schema.path(), fixture_path())
                .unwrap_err()
                .code(),
            "PROFILE_SCHEMA_INVALID"
        );
    }

    #[test]
    fn profile_and_schema_inputs_reject_links_without_following_them() {
        let temporary = tempdir().expect("temporary profile link directory");
        let profile_link = temporary.path().join("profile-link.json");
        symlink_file(&fixture_path(), &profile_link);
        assert_eq!(
            validate_profile(schema_path(), &profile_link)
                .unwrap_err()
                .code(),
            "PROFILE_INVALID"
        );

        let schema_link = temporary.path().join("schema-link.json");
        symlink_file(&schema_path(), &schema_link);
        assert_eq!(
            validate_profile(&schema_link, fixture_path())
                .unwrap_err()
                .code(),
            "PROFILE_SCHEMA_INVALID"
        );
    }

    #[test]
    fn entity_and_key_invariants_are_enforced() {
        let original = read_value(&fixture_path());

        let mut duplicate_entity = original.clone();
        duplicate_entity["entities"][1]["entityId"] = json!("CustomerAccount");
        assert_semantic_invalid(&duplicate_entity, "duplicate entityId");

        let mut no_root = original.clone();
        no_root["entities"][0]["root"] = json!(false);
        assert_semantic_invalid(&no_root, "exactly one root");

        let mut empty_fields = original.clone();
        empty_fields["entities"][0]["fields"] = json!([]);
        assert_semantic_invalid(&empty_fields, "at least one field");

        let mut empty_key = original.clone();
        empty_key["entities"][0]["key"] = json!([]);
        assert_semantic_invalid(&empty_key, "at least one key field");

        let mut missing_key_field = original.clone();
        missing_key_field["entities"][0]["key"] = json!(["unknownField"]);
        assert_semantic_invalid(&missing_key_field, "does not resolve");

        let mut duplicate_field = original;
        duplicate_field["entities"][0]["fields"][1]["fieldId"] = json!("migrationPartnerId");
        assert_semantic_invalid(&duplicate_field, "duplicate fieldId");
    }

    #[test]
    fn relationship_tuple_and_endpoints_are_enforced() {
        let original = read_value(&fixture_path());

        let mut unresolved = original.clone();
        unresolved["relationships"][0]["to"] = json!("MissingEntity");
        assert_semantic_invalid(&unresolved, "does not resolve to an entity");

        let mut duplicate = original;
        let relationship = duplicate["relationships"][0].clone();
        duplicate["relationships"]
            .as_array_mut()
            .expect("relationships should be an array")
            .push(relationship);
        assert_semantic_invalid(&duplicate, "duplicate relationship tuple");
    }

    #[test]
    fn explicit_relationship_keys_are_bounded_required_and_authoritative() {
        let original = read_value(&sales_document_fixture_path());
        validate_value(&original).expect("keyed synthetic relationship should validate");

        let mut optional_endpoint = original.clone();
        optional_endpoint["entities"][1]["fields"][0]["required"] = json!(false);
        assert_semantic_invalid(&optional_endpoint, "required and non-nullable");

        let mut unknown_endpoint = original.clone();
        unknown_endpoint["relationships"][0]["keyPairs"][0]["toFieldId"] = json!("unknownField");
        assert_semantic_invalid(&unknown_endpoint, "does not resolve");

        let mut partial_parent_key = original.clone();
        partial_parent_key["relationships"][0]["keyPairs"][0]["fromFieldId"] = json!("customerId");
        assert_semantic_invalid(&partial_parent_key, "must exactly match");

        let mut unsupported_cardinality = original.clone();
        unsupported_cardinality["relationships"][0]["cardinality"] = json!("1..1");
        assert_semantic_invalid(&unsupported_cardinality, "unsupported cardinality");

        let mut duplicate_id = original;
        let mut relationship = duplicate_id["relationships"][0].clone();
        relationship["cardinality"] = json!("0..*");
        duplicate_id["relationships"]
            .as_array_mut()
            .expect("relationships array")
            .push(relationship);
        assert_semantic_invalid(&duplicate_id, "duplicate relationshipId");
    }

    #[test]
    fn rule_ids_and_structured_targets_are_enforced() {
        let original = read_value(&fixture_path());

        let mut duplicate_id = original.clone();
        let rule = duplicate_id["rules"][0].clone();
        duplicate_id["rules"]
            .as_array_mut()
            .expect("rules should be an array")
            .push(rule);
        assert_semantic_invalid(&duplicate_id, "duplicate ruleId");

        let mut unstructured = original.clone();
        unstructured["rules"][0]["target"] = json!("CustomerAccount.category.extra");
        assert_semantic_invalid(&unstructured, "exactly Entity.field structure");

        let mut unresolved = original;
        unresolved["rules"][0]["target"] = json!("CustomerAccount.unknownField");
        assert_semantic_invalid(&unresolved, "target field");
    }

    #[test]
    fn reference_lookup_rules_share_one_exact_entity_table_version() {
        let mut profile = read_value(&sales_document_fixture_path());
        let reference_rule = profile["rules"]
            .as_array()
            .expect("rules array")
            .iter()
            .find(|rule| rule["kind"] == "reference_lookup")
            .expect("sales profile reference lookup")
            .clone();

        let mut same_entity_same_version = reference_rule.clone();
        same_entity_same_version["ruleId"] = json!("SD-HDR-TYPE-DOMAIN-SECOND");
        same_entity_same_version["target"] = json!("SalesDocument.customerReference");
        profile["rules"]
            .as_array_mut()
            .expect("rules array")
            .push(same_entity_same_version);

        let mut other_entity_other_version = reference_rule;
        other_entity_other_version["ruleId"] = json!("SD-ITEM-PRODUCT-DOMAIN");
        other_entity_other_version["target"] = json!("SalesDocumentItem.productId");
        other_entity_other_version["parameters"]["tableVersion"] = json!("2.0.0");
        profile["rules"]
            .as_array_mut()
            .expect("rules array")
            .push(other_entity_other_version);

        validate_value(&profile).expect(
            "same entity/table/version may serve multiple rules and another entity has independent table authority",
        );
    }

    #[test]
    fn reference_lookup_rules_reject_conflicting_entity_table_versions() {
        let mut profile = read_value(&sales_document_fixture_path());
        let mut conflicting = profile["rules"]
            .as_array()
            .expect("rules array")
            .iter()
            .find(|rule| rule["kind"] == "reference_lookup")
            .expect("sales profile reference lookup")
            .clone();
        conflicting["ruleId"] = json!("SD-HDR-TYPE-DOMAIN-CONFLICT");
        conflicting["target"] = json!("SalesDocument.customerReference");
        conflicting["parameters"]["tableVersion"] = json!("2.0.0");
        profile["rules"]
            .as_array_mut()
            .expect("rules array")
            .push(conflicting);

        assert_semantic_invalid(&profile, "declare conflicting table versions");
    }

    #[test]
    fn canonical_identity_and_issue_text_bounds_are_inclusive() {
        let mut profile = read_value(&fixture_path());
        profile["profileId"] = json!("a".repeat(MAXIMUM_PROFILE_IDENTIFIER_CHARS));
        profile["version"] = json!(format!(
            "{}.{}.{}",
            "1".repeat(10),
            "2".repeat(10),
            "3".repeat(10)
        ));
        assert_eq!(
            profile["version"].as_str().expect("profile version").len(),
            MAXIMUM_PROFILE_VERSION_CHARS
        );
        rename_first_entity(&mut profile, &"E".repeat(MAXIMUM_PROFILE_IDENTIFIER_CHARS));
        profile["entities"][0]["fields"][2]["fieldId"] =
            json!("F".repeat(MAXIMUM_PROFILE_IDENTIFIER_CHARS));
        profile["rules"][0]["ruleId"] = json!("R".repeat(MAXIMUM_PROFILE_IDENTIFIER_CHARS));
        profile["rules"][0]["message"] = json!("m".repeat(MAXIMUM_RULE_MESSAGE_CHARS));

        validate_value(&profile).expect(
            "inclusive identity/version/message bounds must produce schema-valid read evidence",
        );
    }

    #[test]
    fn canonical_schema_rejects_identity_version_and_issue_text_over_bounds() {
        let original = read_value(&fixture_path());
        let mut cases = Vec::new();

        let mut profile_id = original.clone();
        profile_id["profileId"] = json!("a".repeat(MAXIMUM_PROFILE_IDENTIFIER_CHARS + 1));
        cases.push(profile_id);

        let mut version = original.clone();
        version["version"] = json!(format!(
            "{}.{}.{}",
            "1".repeat(11),
            "2".repeat(10),
            "3".repeat(10)
        ));
        assert_eq!(
            version["version"].as_str().expect("profile version").len(),
            MAXIMUM_PROFILE_VERSION_CHARS + 1
        );
        cases.push(version);

        let mut entity_id = original.clone();
        entity_id["entities"][0]["entityId"] =
            json!("E".repeat(MAXIMUM_PROFILE_IDENTIFIER_CHARS + 1));
        cases.push(entity_id);

        let mut field_id = original.clone();
        field_id["entities"][0]["fields"][2]["fieldId"] =
            json!("F".repeat(MAXIMUM_PROFILE_IDENTIFIER_CHARS + 1));
        cases.push(field_id);

        let mut rule_id = original.clone();
        rule_id["rules"][0]["ruleId"] = json!("R".repeat(MAXIMUM_PROFILE_IDENTIFIER_CHARS + 1));
        cases.push(rule_id);

        let mut message = original;
        message["rules"][0]["message"] = json!("m".repeat(MAXIMUM_RULE_MESSAGE_CHARS + 1));
        cases.push(message);

        for profile in cases {
            assert_eq!(
                validate_value(&profile).unwrap_err().code(),
                "PROFILE_INVALID"
            );
        }
    }

    #[test]
    fn runtime_semantics_repeat_downstream_identity_and_issue_text_bounds() {
        let original = read_value(&fixture_path());

        let mut profile_id = original.clone();
        profile_id["profileId"] = json!("a".repeat(MAXIMUM_PROFILE_IDENTIFIER_CHARS + 1));
        assert_runtime_semantic_invalid(&profile_id, "profileId must contain 1..=128 characters");

        let mut version = original.clone();
        version["version"] = json!("1".repeat(MAXIMUM_PROFILE_VERSION_CHARS + 1));
        assert_runtime_semantic_invalid(&version, "profile version must contain 1..=32 characters");

        let mut entity_id = original.clone();
        rename_first_entity(
            &mut entity_id,
            &"E".repeat(MAXIMUM_PROFILE_IDENTIFIER_CHARS + 1),
        );
        assert_runtime_semantic_invalid(&entity_id, "entityId must contain 1..=128 characters");

        let mut field_id = original.clone();
        field_id["entities"][0]["fields"][2]["fieldId"] =
            json!("F".repeat(MAXIMUM_PROFILE_IDENTIFIER_CHARS + 1));
        assert_runtime_semantic_invalid(&field_id, "fieldId must contain 1..=128 characters");

        let mut rule_id = original.clone();
        rule_id["rules"][0]["ruleId"] = json!("R".repeat(MAXIMUM_PROFILE_IDENTIFIER_CHARS + 1));
        assert_runtime_semantic_invalid(&rule_id, "ruleId must contain 1..=128 characters");

        let mut message = original;
        message["rules"][0]["message"] = json!("m".repeat(MAXIMUM_RULE_MESSAGE_CHARS + 1));
        assert_runtime_semantic_invalid(&message, "message must contain 1..=4096 characters");
    }

    #[test]
    fn rule_messages_are_required_nonblank_and_bounded() {
        let original = read_value(&fixture_path());

        let mut missing = original.clone();
        missing["rules"][0]
            .as_object_mut()
            .expect("rule must be an object")
            .remove("message");
        assert!(matches!(
            validate_value(&missing),
            Err(KernelError::ProfileInvalid(_))
        ));

        let mut blank = original.clone();
        blank["rules"][0]["message"] = json!("   ");
        assert_semantic_invalid(&blank, "message must contain 1..=4096 characters");

        let mut oversized_utf8 = original.clone();
        oversized_utf8["rules"][0]["message"] = json!("é".repeat(4096));
        assert_semantic_invalid(&oversized_utf8, "no more than 4096 UTF-8 bytes");

        let mut oversized = original;
        oversized["rules"][0]["message"] = json!("x".repeat(4097));
        assert_eq!(
            validate_value(&oversized).unwrap_err().code(),
            "PROFILE_INVALID"
        );
        assert_runtime_semantic_invalid(&oversized, "message must contain 1..=4096 characters");
    }

    #[test]
    fn adapter_version_and_approved_evidence_are_enforced() {
        let mut blank_adapter_id = read_value(&fixture_path());
        blank_adapter_id["publicationAdapters"][0]["adapterId"] = json!("   ");
        assert_semantic_invalid(&blank_adapter_id, "adapterId must not be empty");

        let mut duplicate_adapter_id = read_value(&fixture_path());
        duplicate_adapter_id["publicationAdapters"][1]["adapterId"] =
            json!("canonical-customer-account-readiness");
        assert_semantic_invalid(&duplicate_adapter_id, "duplicate publication adapterId");

        let mut missing_adapter_version = read_value(&fixture_path());
        missing_adapter_version["publicationAdapters"][0]["version"] = json!("   ");
        assert_semantic_invalid(&missing_adapter_version, "nonempty version");

        let mut no_approval = read_value(&fixture_path());
        no_approval["status"] = json!("APPROVED");
        assert_semantic_invalid(&no_approval, "requires approval evidence");

        let mut blank_evidence = synthetic_approved_profile();
        blank_evidence["approval"]["evidence"] = json!("   ");
        assert_semantic_invalid(&blank_evidence, "nonempty approval.evidence");
    }
}

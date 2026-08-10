//! Canonical JSON and opaque bounded cursor support for WP-003.
//!
//! The canonical form is deliberately small and versioned: object keys are
//! ordered lexicographically, array order is preserved, and `serde_json`
//! emits compact UTF-8.  This is not presented as a general RFC 8785
//! implementation.  It is the Migration Stream canonical JSON v1 contract.

use std::{fmt, fs, path::Path};

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{KernelError, Result};

/// Stable identifier for the canonicalization rules implemented here.
pub const CANONICAL_JSON_VERSION: &str = "migration-stream-canonical-json/v1";

const CURSOR_PREFIX: &str = "v1";
const MAXIMUM_CURSOR_BYTES: usize = 8 * 1024;
const MAXIMUM_CURSOR_CONTRACT_BYTES: usize = 128;
const SHA256_HEX_BYTES: usize = 64;

/// Strict canonical cursor contents.
///
/// `scope_hash` binds a cursor to one immutable query snapshot, while
/// `filter_hash` prevents reuse under a different normalized filter set.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CursorPayload {
    pub contract: String,
    pub scope_hash: String,
    pub filter_hash: String,
    pub last_key: Value,
}

/// Return a recursively key-sorted copy of a JSON value.
pub fn canonicalize_json(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize_json).collect()),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut canonical = Map::new();
            for key in keys {
                canonical.insert(key.clone(), canonicalize_json(&object[key]));
            }
            Value::Object(canonical)
        }
        primitive => primitive.clone(),
    }
}

/// Serialize a JSON value using Migration Stream canonical JSON v1.
///
/// # Errors
///
/// Returns a JSON serialization error through the crate error contract.
pub fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>> {
    serde_json::to_vec(&canonicalize_json(value)).map_err(KernelError::from)
}

/// Serialize a JSON value to a canonical compact UTF-8 string.
///
/// # Errors
///
/// Returns a JSON serialization error through the crate error contract.
pub fn canonical_json_string(value: &Value) -> Result<String> {
    String::from_utf8(canonical_json_bytes(value)?).map_err(|error| {
        KernelError::Reconciliation(format!(
            "canonical JSON serialization produced invalid UTF-8: {error}"
        ))
    })
}

/// Return lowercase hexadecimal SHA-256 for arbitrary bytes.
pub fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

/// Hash the canonical JSON representation of a value.
///
/// # Errors
///
/// Returns a JSON serialization error through the crate error contract.
pub fn canonical_json_sha256(value: &Value) -> Result<String> {
    Ok(sha256_hex(canonical_json_bytes(value)?))
}

/// Compatibility name used by the WP-003 registry, mapping, and validation
/// layers for the canonical semantic hash contract.
///
/// # Errors
///
/// Returns a JSON serialization error through the crate error contract.
pub fn canonical_hash(value: &Value) -> Result<String> {
    canonical_json_sha256(value)
}

/// Read one JSON file while rejecting duplicate object keys at every depth.
///
/// # Errors
///
/// Returns a typed I/O or JSON error. Duplicate keys are reported as JSON
/// errors and cannot be hidden by last-key-wins parsing.
pub fn read_unique_json(path: impl AsRef<Path>) -> Result<Value> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|error| KernelError::io(path, error))?;
    parse_unique_json(&bytes)
}

/// Parse JSON bytes while rejecting duplicate object keys at every depth.
///
/// # Errors
///
/// Returns a typed JSON error for malformed input, duplicate keys, or trailing
/// non-whitespace data.
pub fn parse_unique_json(bytes: &[u8]) -> Result<Value> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let UniqueJsonValue(value) = UniqueJsonValue::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

/// Encode a strictly scoped, checksummed cursor.
///
/// # Errors
///
/// Rejects invalid contract tokens, non-SHA-256 scope/filter identities, or an
/// oversized encoded cursor.
pub fn encode_cursor(
    contract: &str,
    scope_hash: &str,
    filter_hash: &str,
    last_key: Value,
) -> Result<String> {
    validate_contract(contract)?;
    validate_sha256(scope_hash, "cursor scope hash")?;
    validate_sha256(filter_hash, "cursor filter hash")?;
    validate_last_key(&last_key)?;
    let payload = CursorPayload {
        contract: contract.to_owned(),
        scope_hash: scope_hash.to_owned(),
        filter_hash: filter_hash.to_owned(),
        last_key,
    };
    let payload_bytes = canonical_json_bytes(&serde_json::to_value(payload)?)?;
    let checksum = sha256_hex(&payload_bytes);
    let encoded = format!(
        "{CURSOR_PREFIX}.{}.{}",
        encode_hex(&payload_bytes),
        checksum
    );
    if encoded.len() > MAXIMUM_CURSOR_BYTES {
        return cursor_error(format!(
            "encoded cursor is {} bytes; maximum is {MAXIMUM_CURSOR_BYTES}",
            encoded.len()
        ));
    }
    Ok(encoded)
}

/// Decode and validate a cursor against an exact contract, scope, and filter.
///
/// # Errors
///
/// Rejects malformed, noncanonical, oversized, checksum-mismatched,
/// cross-contract, stale-scope, or cross-filter cursors.
pub fn decode_cursor(
    cursor: &str,
    expected_contract: &str,
    expected_scope_hash: &str,
    expected_filter_hash: &str,
) -> Result<CursorPayload> {
    validate_contract(expected_contract)?;
    validate_sha256(expected_scope_hash, "expected cursor scope hash")?;
    validate_sha256(expected_filter_hash, "expected cursor filter hash")?;
    if cursor.is_empty() || cursor.len() > MAXIMUM_CURSOR_BYTES {
        return cursor_error(format!(
            "cursor length must be in 1..={MAXIMUM_CURSOR_BYTES} bytes"
        ));
    }
    let mut components = cursor.split('.');
    let prefix = components.next();
    let encoded_payload = components.next();
    let checksum = components.next();
    if prefix != Some(CURSOR_PREFIX)
        || encoded_payload.is_none()
        || checksum.is_none()
        || components.next().is_some()
    {
        return cursor_error("cursor must have exactly v1.<payload>.<checksum> form");
    }
    let encoded_payload = encoded_payload.unwrap_or_default();
    let checksum = checksum.unwrap_or_default();
    validate_sha256(checksum, "cursor checksum")?;
    let payload_bytes = decode_hex(encoded_payload)?;
    if sha256_hex(&payload_bytes) != checksum {
        return cursor_error("cursor checksum does not match its payload");
    }
    let value = parse_unique_json(&payload_bytes).map_err(|error| {
        KernelError::Reconciliation(format!("invalid cursor payload JSON: {error}"))
    })?;
    if canonical_json_bytes(&value)? != payload_bytes {
        return cursor_error("cursor payload is not canonical JSON v1");
    }
    let payload: CursorPayload = serde_json::from_value(value).map_err(|error| {
        KernelError::Reconciliation(format!("invalid cursor payload contract: {error}"))
    })?;
    validate_contract(&payload.contract)?;
    validate_sha256(&payload.scope_hash, "cursor scope hash")?;
    validate_sha256(&payload.filter_hash, "cursor filter hash")?;
    validate_last_key(&payload.last_key)?;
    if payload.contract != expected_contract {
        return cursor_error(format!(
            "cursor contract {:?} does not match expected contract {:?}",
            payload.contract, expected_contract
        ));
    }
    if payload.scope_hash != expected_scope_hash {
        return cursor_error("cursor scope is stale or belongs to a different snapshot");
    }
    if payload.filter_hash != expected_filter_hash {
        return cursor_error("cursor filters do not match the current request");
    }
    Ok(payload)
}

fn validate_contract(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_CURSOR_CONTRACT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
    {
        return cursor_error("cursor contract must be a bounded ASCII letter/digit/._-/ token");
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != SHA256_HEX_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return cursor_error(format!(
            "{label} must be 64 lowercase hexadecimal characters"
        ));
    }
    Ok(())
}

fn validate_last_key(value: &Value) -> Result<()> {
    let values = value.as_array().ok_or_else(|| {
        KernelError::Reconciliation("cursor last_key must be a JSON array".to_owned())
    })?;
    if values.is_empty() || values.len() > 8 {
        return cursor_error("cursor last_key must contain between one and eight values");
    }
    for item in values {
        match item {
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
            Value::String(text) if text.len() <= 2_048 => {}
            Value::String(_) => {
                return cursor_error("cursor last_key string exceeds 2,048 bytes");
            }
            Value::Array(_) | Value::Object(_) => {
                return cursor_error("cursor last_key values must be scalar JSON values");
            }
        }
    }
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if value.is_empty() || !value.len().is_multiple_of(2) {
        return cursor_error("cursor payload must be nonempty even-length hexadecimal");
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let high = decode_nibble(pair[0])?;
        let low = decode_nibble(pair[1])?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

fn decode_nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => cursor_error("cursor payload must use lowercase hexadecimal"),
    }
}

fn cursor_error<T>(detail: impl Into<String>) -> Result<T> {
    Err(KernelError::Reconciliation(format!(
        "invalid practitioner cursor: {}",
        detail.into()
    )))
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
                let mut values = Map::new();
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

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn canonical_json_sorts_objects_and_preserves_array_order() {
        let input = json!({
            "z": {"beta": 2, "alpha": "α"},
            "array": [{"b": 2, "a": 1}, 3, 2, 1]
        });
        let canonical = canonical_json_string(&input).expect("canonical JSON");
        assert_eq!(
            canonical,
            r#"{"array":[{"a":1,"b":2},3,2,1],"z":{"alpha":"α","beta":2}}"#
        );
        assert_eq!(
            canonical_json_sha256(&json!({"b": 2, "a": 1})).expect("canonical hash"),
            "43258cff783fe7036d8a43033f830adfc60ec037382473548ac742b888292777"
        );
    }

    #[test]
    fn strict_reader_rejects_duplicate_keys_at_nested_depth() {
        let mut file = NamedTempFile::new().expect("temporary JSON");
        file.write_all(br#"{"outer":{"value":1,"value":2}}"#)
            .expect("write duplicate JSON");
        let error = read_unique_json(file.path()).expect_err("duplicate key must fail");
        assert!(error.to_string().contains("duplicate JSON object key"));
    }

    #[test]
    fn cursor_round_trip_binds_contract_scope_and_filters() {
        let scope = sha256_hex(b"immutable-scope");
        let filters = sha256_hex(b"normalized-filters");
        let cursor = encode_cursor("issue-list/v1", &scope, &filters, json!(["issue-0001"]))
            .expect("encode cursor");
        let decoded =
            decode_cursor(&cursor, "issue-list/v1", &scope, &filters).expect("decode cursor");
        assert_eq!(decoded.last_key, json!(["issue-0001"]));

        let other_scope = sha256_hex(b"other-scope");
        assert!(decode_cursor(&cursor, "issue-list/v1", &other_scope, &filters).is_err());
        let other_filters = sha256_hex(b"other-filters");
        assert!(decode_cursor(&cursor, "issue-list/v1", &scope, &other_filters).is_err());
        assert!(decode_cursor(&cursor, "file-manifest/v1", &scope, &filters).is_err());
    }

    #[test]
    fn cursor_checksum_and_canonical_payload_are_strict() {
        let scope = sha256_hex(b"scope");
        let filters = sha256_hex(b"filters");
        let cursor = encode_cursor("file-manifest/v1", &scope, &filters, json!(["a.csv"]))
            .expect("encode cursor");
        let mut tampered = cursor.into_bytes();
        let payload_offset = CURSOR_PREFIX.len() + 1;
        tampered[payload_offset] = if tampered[payload_offset] == b'a' {
            b'b'
        } else {
            b'a'
        };
        let tampered = String::from_utf8(tampered).expect("ASCII cursor");
        assert!(decode_cursor(&tampered, "file-manifest/v1", &scope, &filters).is_err());
    }
}

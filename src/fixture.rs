//! Deterministic, streaming synthetic ZIP/CSV fixtures for WP-002 evidence.
//!
//! The generator is deliberately object-neutral. It produces textual CSV data
//! only and does not infer business meaning, types, mappings, or compatibility.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use csv::{Terminator, WriterBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zip::{CompressionMethod, System, ZipWriter, write::SimpleFileOptions};

use crate::{KernelError, Result, durability::publish_new};

pub const FIXTURE_SCHEMA_VERSION: u32 = 1;
pub const GENERATOR_NAME: &str = "migration-stream-synthetic-fixture";
pub const GENERATOR_VERSION: &str = "wp-002-v1";

const MAXIMUM_FILE_COUNT: u32 = 1_000;
const MAXIMUM_COLUMNS: u32 = 4_096;
const MAXIMUM_ROWS_PER_FILE: u64 = 10_000_000;
const MAXIMUM_TOTAL_ROWS: u64 = 10_000_000;
const MAXIMUM_FIELD_LENGTH: usize = 1024 * 1024;
const MAXIMUM_FIXTURE_NAME_BYTES: usize = 64;
const HASH_BUFFER_BYTES: usize = 16 * 1024;
const MINIMUM_ARCHIVE_OUTPUT_CEILING: u64 = 256;

/// Minimum, typical, and maximum generated field sizes, measured in UTF-8 bytes.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FieldLengthSpec {
    pub minimum: usize,
    pub typical: usize,
    pub maximum: usize,
}

/// Controls how repetitive the non-key field payload is.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Compressibility {
    Low,
    Medium,
    High,
}

/// Controls the deterministic value domain of non-key columns.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    Low,
    Medium,
    High,
}

/// Frequency of a hot value in non-key columns.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SkewSpec {
    pub hot_value_frequency: f64,
}

/// Intentionally malformed CSV syntax emitted only into a separate archive.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MalformedKind {
    InconsistentColumns,
    UnterminatedQuote,
    InvalidUtf8,
}

/// Optional request for a separate negative-test archive.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MalformedSpec {
    pub kind: MalformedKind,
}

/// Version-controlled parameters for a bounded synthetic fixture.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FixtureSpec {
    pub schema_version: u32,
    pub fixture_name: String,
    pub seed: u64,
    pub file_count: u32,
    pub rows_per_file: u64,
    pub columns: u32,
    pub field_lengths: FieldLengthSpec,
    pub compressibility: Compressibility,
    pub empty_frequency: f64,
    pub cardinality: Cardinality,
    pub duplicate_key_frequency: f64,
    pub multiline_frequency: f64,
    pub delimiter: String,
    pub delimiter_in_field_frequency: f64,
    pub escaped_quote_frequency: f64,
    pub unicode_frequency: f64,
    pub leading_zero_id_frequency: f64,
    pub skew: SkewSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub malformed: Option<MalformedSpec>,
    pub maximum_expanded_bytes: u64,
    pub maximum_output_bytes: u64,
}

impl FixtureSpec {
    /// Load and validate a fixture specification from JSON.
    ///
    /// # Errors
    ///
    /// Returns a typed JSON, I/O, or validation error. Unknown JSON fields are
    /// rejected so evidence cannot silently omit a misspelled parameter.
    pub fn from_path(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).map_err(|error| KernelError::io(path, error))?;
        let spec: Self = serde_json::from_slice(&bytes)?;
        spec.validate()?;
        Ok(spec)
    }

    /// Validate safety ceilings and all deterministic generation parameters.
    ///
    /// # Errors
    ///
    /// Returns `POLICY_INVALID` for an invalid or unbounded specification.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != FIXTURE_SCHEMA_VERSION {
            return invalid_spec(format!(
                "fixture schema_version must be {FIXTURE_SCHEMA_VERSION}"
            ));
        }
        validate_fixture_name(&self.fixture_name)?;
        if self.file_count == 0 || self.file_count > MAXIMUM_FILE_COUNT {
            return invalid_spec(format!("file_count must be in 1..={MAXIMUM_FILE_COUNT}"));
        }
        if self.rows_per_file == 0 || self.rows_per_file > MAXIMUM_ROWS_PER_FILE {
            return invalid_spec(format!(
                "rows_per_file must be in 1..={MAXIMUM_ROWS_PER_FILE}"
            ));
        }
        let total_rows = u64::from(self.file_count)
            .checked_mul(self.rows_per_file)
            .ok_or_else(|| invalid_spec_error("total row count overflows u64"))?;
        if total_rows > MAXIMUM_TOTAL_ROWS {
            return invalid_spec(format!("total rows must not exceed {MAXIMUM_TOTAL_ROWS}"));
        }
        if self.columns == 0 || self.columns > MAXIMUM_COLUMNS {
            return invalid_spec(format!("columns must be in 1..={MAXIMUM_COLUMNS}"));
        }
        if self.field_lengths.maximum > MAXIMUM_FIELD_LENGTH
            || self.field_lengths.minimum > self.field_lengths.typical
            || self.field_lengths.typical > self.field_lengths.maximum
        {
            return invalid_spec(format!(
                "field lengths must satisfy minimum <= typical <= maximum <= {MAXIMUM_FIELD_LENGTH}"
            ));
        }
        if self.maximum_expanded_bytes == 0
            || self.maximum_output_bytes < MINIMUM_ARCHIVE_OUTPUT_CEILING
        {
            return invalid_spec(format!(
                "maximum_expanded_bytes must be positive and maximum_output_bytes must be at least {MINIMUM_ARCHIVE_OUTPUT_CEILING}"
            ));
        }
        for (name, frequency) in self.frequencies() {
            if !frequency.is_finite() || !(0.0..=1.0).contains(&frequency) {
                return invalid_spec(format!("{name} must be finite and in 0..=1"));
            }
        }
        let delimiter = self.delimiter.as_bytes();
        if delimiter.len() != 1
            || !delimiter[0].is_ascii()
            || matches!(delimiter[0], 0 | b'\r' | b'\n' | b'"')
        {
            return invalid_spec(
                "delimiter must be one non-NUL ASCII byte other than CR, LF, or quote".to_owned(),
            );
        }
        Ok(())
    }

    fn frequencies(&self) -> [(&'static str, f64); 7] {
        [
            ("empty_frequency", self.empty_frequency),
            ("duplicate_key_frequency", self.duplicate_key_frequency),
            ("multiline_frequency", self.multiline_frequency),
            (
                "delimiter_in_field_frequency",
                self.delimiter_in_field_frequency,
            ),
            ("escaped_quote_frequency", self.escaped_quote_frequency),
            ("unicode_frequency", self.unicode_frequency),
            ("leading_zero_id_frequency", self.leading_zero_id_frequency),
        ]
    }

    fn delimiter_byte(&self) -> u8 {
        self.delimiter.as_bytes()[0]
    }
}

/// Deterministic expanded-file evidence for one ZIP entry.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FixtureFileManifest {
    pub path: String,
    pub rows: u64,
    pub columns: u32,
    pub expanded_bytes: u64,
    pub sha256: String,
    pub intentionally_malformed: bool,
}

/// Evidence for one generated archive.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FixtureArchiveManifest {
    pub file_name: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub files: Vec<FixtureFileManifest>,
}

/// Sidecar evidence written next to the generated archive(s).
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct FixtureManifest {
    pub schema_version: u32,
    pub generator_name: String,
    pub generator_version: String,
    pub fixture_identity: String,
    pub seed: u64,
    pub spec: FixtureSpec,
    pub valid_archive: FixtureArchiveManifest,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub malformed_archive: Option<FixtureArchiveManifest>,
    pub total_expanded_bytes: u64,
    pub total_output_bytes: u64,
}

/// Paths and evidence returned after all outputs are durably created.
#[derive(Clone, Debug, Serialize)]
pub struct GeneratedFixture {
    pub archive_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub malformed_archive_path: Option<PathBuf>,
    pub manifest_path: PathBuf,
    pub manifest: FixtureManifest,
}

/// Generate deterministic ZIP/CSV fixture data without holding the data set in memory.
///
/// Output paths use create-new semantics. Existing archives or manifests are
/// never opened for writing. A failed generation removes only files created by
/// the current call.
///
/// # Errors
///
/// Returns a typed validation, I/O, CSV, ZIP, or safety-ceiling error. Partial
/// outputs created by this call are cleaned up on error.
pub fn generate_fixture(spec: &FixtureSpec, output_directory: &Path) -> Result<GeneratedFixture> {
    spec.validate()?;
    fs::create_dir_all(output_directory)
        .map_err(|error| KernelError::io(output_directory, error))?;

    let archive_path = output_directory.join(format!("{}.zip", spec.fixture_name));
    let malformed_archive_path = spec
        .malformed
        .as_ref()
        .map(|_| output_directory.join(format!("{}.malformed.zip", spec.fixture_name)));
    let manifest_path = output_directory.join(format!("{}.manifest.json", spec.fixture_name));
    ensure_paths_absent(
        std::iter::once(archive_path.as_path())
            .chain(malformed_archive_path.as_deref())
            .chain(std::iter::once(manifest_path.as_path())),
    )?;

    let mut cleanup = CreatedOutputGuard::default();
    let archive_staging = fixture_staging_path(output_directory);
    let archive_file = create_new_file(&archive_staging)?;
    cleanup.register(archive_staging.clone());
    let mut rng = SplitMix64::new(spec.seed);
    let mut valid_archive = write_valid_archive(spec, &archive_staging, archive_file, &mut rng)?;
    publish_new(&archive_staging, &archive_path)
        .map_err(|error| KernelError::io(&archive_path, error))?;
    cleanup.register(archive_path.clone());
    valid_archive.file_name = output_file_name(&archive_path);
    let mut total_expanded_bytes = expanded_bytes(&valid_archive)?;
    let mut total_output_bytes = valid_archive.size_bytes;

    let malformed_archive = if let (Some(malformed), Some(path)) =
        (spec.malformed.as_ref(), malformed_archive_path.as_ref())
    {
        let remaining_expanded = spec
            .maximum_expanded_bytes
            .checked_sub(total_expanded_bytes)
            .ok_or_else(|| ceiling_error("expanded byte ceiling exceeded"))?;
        let remaining_output = spec
            .maximum_output_bytes
            .checked_sub(total_output_bytes)
            .ok_or_else(|| ceiling_error("output byte ceiling exceeded"))?;
        let staging = fixture_staging_path(output_directory);
        let file = create_new_file(&staging)?;
        cleanup.register(staging.clone());
        let mut archive = write_malformed_archive(
            spec,
            malformed,
            &staging,
            file,
            remaining_expanded,
            remaining_output,
        )?;
        publish_new(&staging, path).map_err(|error| KernelError::io(path, error))?;
        cleanup.register(path.clone());
        archive.file_name = output_file_name(path);
        total_expanded_bytes = total_expanded_bytes
            .checked_add(expanded_bytes(&archive)?)
            .ok_or_else(|| ceiling_error("expanded byte count overflow"))?;
        total_output_bytes = total_output_bytes
            .checked_add(archive.size_bytes)
            .ok_or_else(|| ceiling_error("output byte count overflow"))?;
        Some(archive)
    } else {
        None
    };

    let fixture_identity = fixture_identity(spec, &valid_archive, malformed_archive.as_ref())?;
    let manifest = FixtureManifest {
        schema_version: FIXTURE_SCHEMA_VERSION,
        generator_name: GENERATOR_NAME.to_owned(),
        generator_version: GENERATOR_VERSION.to_owned(),
        fixture_identity,
        seed: spec.seed,
        spec: spec.clone(),
        valid_archive,
        malformed_archive,
        total_expanded_bytes,
        total_output_bytes,
    };

    let manifest_staging = fixture_staging_path(output_directory);
    let mut manifest_file = create_new_file(&manifest_staging)?;
    cleanup.register(manifest_staging.clone());
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    manifest_bytes.push(b'\n');
    manifest_file
        .write_all(&manifest_bytes)
        .map_err(|error| KernelError::io(&manifest_staging, error))?;
    manifest_file
        .sync_all()
        .map_err(|error| KernelError::io(&manifest_staging, error))?;
    drop(manifest_file);
    publish_new(&manifest_staging, &manifest_path)
        .map_err(|error| KernelError::io(&manifest_path, error))?;
    cleanup.register(manifest_path.clone());
    cleanup.commit();

    Ok(GeneratedFixture {
        archive_path,
        malformed_archive_path,
        manifest_path,
        manifest,
    })
}

fn write_valid_archive(
    spec: &FixtureSpec,
    archive_path: &Path,
    archive_file: File,
    rng: &mut SplitMix64,
) -> Result<FixtureArchiveManifest> {
    let capped_file = CappedFile::new(archive_file, spec.maximum_output_bytes, archive_path);
    let mut zip = ZipWriter::new(capped_file);
    let options = deterministic_file_options();
    let mut files = Vec::with_capacity(spec.file_count as usize);
    let mut expanded_so_far = 0_u64;

    for file_index in 0..spec.file_count {
        let path = format!("data/file-{:04}.csv", file_index + 1);
        zip.start_file(&path, options)
            .map_err(|error| zip_error(archive_path, error))?;
        let remaining = spec
            .maximum_expanded_bytes
            .checked_sub(expanded_so_far)
            .ok_or_else(|| ceiling_error("expanded byte ceiling exceeded"))?;
        let entry_writer = EntryDigestWriter::new(&mut zip, remaining, archive_path);
        let mut csv = WriterBuilder::new()
            .delimiter(spec.delimiter_byte())
            .terminator(Terminator::Any(b'\n'))
            .from_writer(entry_writer);

        let headers = headers(spec.columns);
        csv.write_record(&headers)
            .map_err(|error| csv_error(archive_path, error))?;
        let mut previous_key = String::new();
        for row_index in 0..spec.rows_per_file {
            let record = record(spec, rng, file_index, row_index, &previous_key);
            previous_key.clone_from(&record[0]);
            csv.write_record(&record)
                .map_err(|error| csv_error(archive_path, error))?;
        }
        csv.flush()
            .map_err(|error| KernelError::io(archive_path, error))?;
        let entry_writer = csv
            .into_inner()
            .map_err(|error| KernelError::io(archive_path, error.into_error()))?;
        let (expanded_bytes, sha256) = entry_writer.finish();
        expanded_so_far = expanded_so_far
            .checked_add(expanded_bytes)
            .ok_or_else(|| ceiling_error("expanded byte count overflow"))?;
        files.push(FixtureFileManifest {
            path,
            rows: spec.rows_per_file,
            columns: spec.columns,
            expanded_bytes,
            sha256,
            intentionally_malformed: false,
        });
    }

    let capped_file = zip
        .finish()
        .map_err(|error| zip_error(archive_path, error))?;
    capped_file
        .sync_all()
        .map_err(|error| KernelError::io(archive_path, error))?;
    let size_bytes = capped_file.length();
    drop(capped_file);
    let sha256 = file_sha256(archive_path)?;
    Ok(FixtureArchiveManifest {
        file_name: archive_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        size_bytes,
        sha256,
        files,
    })
}

fn write_malformed_archive(
    spec: &FixtureSpec,
    malformed: &MalformedSpec,
    archive_path: &Path,
    archive_file: File,
    maximum_expanded_bytes: u64,
    maximum_output_bytes: u64,
) -> Result<FixtureArchiveManifest> {
    let capped_file = CappedFile::new(archive_file, maximum_output_bytes, archive_path);
    let mut zip = ZipWriter::new(capped_file);
    let path = "malformed/malformed.csv".to_owned();
    zip.start_file(&path, deterministic_file_options())
        .map_err(|error| zip_error(archive_path, error))?;
    let bytes = malformed_bytes(spec, malformed.kind);
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_expanded_bytes {
        return Err(ceiling_error("expanded byte ceiling exceeded"));
    }
    let mut entry_writer = EntryDigestWriter::new(&mut zip, maximum_expanded_bytes, archive_path);
    entry_writer
        .write_all(&bytes)
        .map_err(|error| KernelError::io(archive_path, error))?;
    let (expanded_bytes, entry_sha256) = entry_writer.finish();
    let capped_file = zip
        .finish()
        .map_err(|error| zip_error(archive_path, error))?;
    capped_file
        .sync_all()
        .map_err(|error| KernelError::io(archive_path, error))?;
    let size_bytes = capped_file.length();
    drop(capped_file);
    let sha256 = file_sha256(archive_path)?;
    Ok(FixtureArchiveManifest {
        file_name: archive_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        size_bytes,
        sha256,
        files: vec![FixtureFileManifest {
            path,
            rows: 1,
            columns: spec.columns,
            expanded_bytes,
            sha256: entry_sha256,
            intentionally_malformed: true,
        }],
    })
}

fn deterministic_file_options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .system(System::Unix)
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6))
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644)
}

fn headers(columns: u32) -> Vec<String> {
    let mut headers = Vec::with_capacity(columns as usize);
    headers.push("id".to_owned());
    for column in 1..columns {
        headers.push(format!("column_{:04}", column + 1));
    }
    headers
}

fn record(
    spec: &FixtureSpec,
    rng: &mut SplitMix64,
    file_index: u32,
    row_index: u64,
    previous_key: &str,
) -> Vec<String> {
    let mut fields = Vec::with_capacity(spec.columns as usize);
    let duplicate = row_index > 0 && rng.event(spec.duplicate_key_frequency);
    if duplicate {
        fields.push(previous_key.to_owned());
    } else {
        fields.push(key_value(spec, rng, file_index, row_index));
    }
    for column_index in 1..spec.columns {
        if rng.event(spec.empty_frequency) {
            fields.push(String::new());
        } else {
            fields.push(field_value(spec, rng, file_index, row_index, column_index));
        }
    }
    fields
}

fn key_value(spec: &FixtureSpec, rng: &mut SplitMix64, file_index: u32, row_index: u64) -> String {
    let numeric = u64::from(file_index)
        .saturating_mul(spec.rows_per_file)
        .saturating_add(row_index)
        .saturating_add(1);
    if rng.event(spec.leading_zero_id_frequency) {
        let width = decimal_width(
            u64::from(spec.file_count)
                .saturating_mul(spec.rows_per_file)
                .max(10),
        );
        format!("K{numeric:0width$}")
    } else {
        format!("K{numeric}")
    }
}

fn decimal_width(mut value: u64) -> usize {
    let mut width = 1;
    while value >= 10 {
        value /= 10;
        width += 1;
    }
    width
}

fn field_value(
    spec: &FixtureSpec,
    rng: &mut SplitMix64,
    file_index: u32,
    row_index: u64,
    column_index: u32,
) -> String {
    let target_length = target_length(spec, row_index, column_index);
    if target_length == 0 {
        return String::new();
    }

    let hot = rng.event(spec.skew.hot_value_frequency);
    let domain_value = if hot {
        0
    } else {
        match spec.cardinality {
            Cardinality::Low => rng.bounded(8),
            Cardinality::Medium => rng.bounded(1_024),
            Cardinality::High => u64::from(file_index)
                .saturating_mul(spec.rows_per_file)
                .saturating_add(row_index),
        }
    };
    let mut prefix = format!("C{column_index:04}V{domain_value}");
    if rng.event(spec.multiline_frequency) {
        prefix.push('\n');
    }
    if rng.event(spec.delimiter_in_field_frequency) {
        prefix.push(char::from(spec.delimiter_byte()));
    }
    if rng.event(spec.escaped_quote_frequency) {
        prefix.push('"');
    }
    if rng.event(spec.unicode_frequency) {
        prefix.push_str("Δ東");
    }
    fit_payload(
        &prefix,
        target_length,
        spec.compressibility,
        spec.delimiter_byte(),
        rng,
    )
}

fn target_length(spec: &FixtureSpec, row_index: u64, column_index: u32) -> usize {
    match (row_index + u64::from(column_index)) % 16 {
        0 => spec.field_lengths.minimum,
        1 => spec.field_lengths.maximum,
        _ => spec.field_lengths.typical,
    }
}

fn fit_payload(
    prefix: &str,
    target_length: usize,
    compressibility: Compressibility,
    delimiter: u8,
    rng: &mut SplitMix64,
) -> String {
    let mut value = truncate_utf8(prefix, target_length).to_owned();
    while value.len() < target_length {
        let remaining = target_length - value.len();
        let mut byte = match compressibility {
            Compressibility::High => b'A',
            Compressibility::Medium => b'a' + u8::try_from(value.len() % 4).unwrap_or(0),
            Compressibility::Low => {
                const ALPHABET: &[u8] =
                    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
                let domain = u64::try_from(ALPHABET.len()).unwrap_or(1);
                let index = usize::try_from(rng.bounded(domain)).unwrap_or(0);
                ALPHABET[index]
            }
        };
        if byte == delimiter {
            byte = if byte == b'Z' { b'Y' } else { b'Z' };
        }
        if remaining == 0 {
            break;
        }
        value.push(char::from(byte));
    }
    value
}

fn truncate_utf8(value: &str, maximum_bytes: usize) -> &str {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &value[..boundary]
}

fn malformed_bytes(spec: &FixtureSpec, kind: MalformedKind) -> Vec<u8> {
    let delimiter = spec.delimiter_byte();
    let mut value = raw_header(spec, delimiter);
    match kind {
        MalformedKind::InconsistentColumns => {
            value.extend_from_slice(b"K1");
            for _ in 0..spec.columns {
                value.push(delimiter);
                value.extend_from_slice(b"extra");
            }
            value.push(b'\n');
            value
        }
        MalformedKind::UnterminatedQuote => {
            value.extend_from_slice(b"K1");
            value.push(delimiter);
            value.extend_from_slice(b"\"unterminated\n");
            value
        }
        MalformedKind::InvalidUtf8 => {
            if spec.columns == 1 {
                value.extend_from_slice(&[0xff, b'\n']);
            } else {
                value.extend_from_slice(b"K1");
                for column in 1..spec.columns {
                    value.push(delimiter);
                    if column + 1 == spec.columns {
                        value.push(0xff);
                    } else {
                        value.extend_from_slice(b"value");
                    }
                }
                value.push(b'\n');
            }
            value
        }
    }
}

fn raw_header(spec: &FixtureSpec, delimiter: u8) -> Vec<u8> {
    let mut value = Vec::new();
    for (index, header) in headers(spec.columns).iter().enumerate() {
        if index > 0 {
            value.push(delimiter);
        }
        value.extend_from_slice(header.as_bytes());
    }
    value.push(b'\n');
    value
}

#[derive(Serialize)]
struct FixtureIdentity<'a> {
    generator_name: &'a str,
    generator_version: &'a str,
    spec: &'a FixtureSpec,
    valid_archive: &'a FixtureArchiveManifest,
    malformed_archive: Option<&'a FixtureArchiveManifest>,
}

fn fixture_identity(
    spec: &FixtureSpec,
    valid_archive: &FixtureArchiveManifest,
    malformed_archive: Option<&FixtureArchiveManifest>,
) -> Result<String> {
    let identity = FixtureIdentity {
        generator_name: GENERATOR_NAME,
        generator_version: GENERATOR_VERSION,
        spec,
        valid_archive,
        malformed_archive,
    };
    let bytes = serde_json::to_vec(&identity)?;
    Ok(hex_digest(Sha256::digest(bytes)))
}

fn expanded_bytes(archive: &FixtureArchiveManifest) -> Result<u64> {
    archive.files.iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.expanded_bytes)
            .ok_or_else(|| ceiling_error("expanded byte count overflow"))
    })
}

fn file_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|error| KernelError::io(path, error))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| KernelError::io(path, error))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(hex_digest(digest.finalize()))
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;

    let bytes = bytes.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing hexadecimal to String cannot fail");
    }
    output
}

fn ensure_paths_absent<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Result<()> {
    for path in paths {
        match fs::symlink_metadata(path) {
            Ok(_) => {
                return Err(KernelError::io(
                    path,
                    io::Error::new(io::ErrorKind::AlreadyExists, "output already exists"),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(KernelError::io(path, error)),
        }
    }
    Ok(())
}

fn create_new_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| KernelError::io(path, error))
}

fn fixture_staging_path(output_directory: &Path) -> PathBuf {
    output_directory.join(format!(".migration-stream-fixture-{}.tmp", Uuid::new_v4()))
}

fn output_file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn validate_fixture_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > MAXIMUM_FIXTURE_NAME_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return invalid_spec(format!(
            "fixture_name must be 1..={MAXIMUM_FIXTURE_NAME_BYTES} ASCII letters, digits, '-' or '_'"
        ));
    }
    Ok(())
}

fn invalid_spec<T>(message: String) -> Result<T> {
    Err(invalid_spec_error(message))
}

fn invalid_spec_error(message: impl Into<String>) -> KernelError {
    KernelError::PolicyInvalid(format!("synthetic fixture: {}", message.into()))
}

fn ceiling_error(message: impl Into<String>) -> KernelError {
    invalid_spec_error(message)
}

fn zip_error(path: &Path, error: zip::result::ZipError) -> KernelError {
    KernelError::io(path, io::Error::other(error))
}

fn csv_error(path: &Path, error: csv::Error) -> KernelError {
    KernelError::io(path, io::Error::other(error))
}

#[derive(Default)]
struct CreatedOutputGuard {
    paths: Vec<PathBuf>,
    committed: bool,
}

impl CreatedOutputGuard {
    fn register(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for CreatedOutputGuard {
    fn drop(&mut self) {
        if !self.committed {
            for path in &self.paths {
                let _ = fs::remove_file(path);
            }
        }
    }
}

struct CappedFile {
    inner: File,
    maximum_length: u64,
    position: u64,
    length: u64,
    path: PathBuf,
}

impl CappedFile {
    fn new(inner: File, maximum_length: u64, path: &Path) -> Self {
        Self {
            inner,
            maximum_length,
            position: 0,
            length: 0,
            path: path.to_path_buf(),
        }
    }

    const fn length(&self) -> u64 {
        self.length
    }

    fn sync_all(&self) -> io::Result<()> {
        self.inner.sync_all()
    }

    fn limit_error(&self) -> io::Error {
        io::Error::other(format!(
            "output byte ceiling {} exceeded for {}",
            self.maximum_length,
            self.path.display()
        ))
    }
}

impl Write for CappedFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let requested = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        let new_position = self
            .position
            .checked_add(requested)
            .ok_or_else(|| self.limit_error())?;
        if new_position > self.maximum_length {
            return Err(self.limit_error());
        }
        let count = self.inner.write(buffer)?;
        self.position = self
            .position
            .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
            .ok_or_else(|| self.limit_error())?;
        self.length = self.length.max(self.position);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Seek for CappedFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let position = self.inner.seek(position)?;
        if position > self.maximum_length {
            return Err(self.limit_error());
        }
        self.position = position;
        Ok(position)
    }
}

struct EntryDigestWriter<'a, W: Write> {
    inner: &'a mut W,
    maximum_bytes: u64,
    count: u64,
    digest: Sha256,
    path: PathBuf,
}

impl<'a, W: Write> EntryDigestWriter<'a, W> {
    fn new(inner: &'a mut W, maximum_bytes: u64, path: &Path) -> Self {
        Self {
            inner,
            maximum_bytes,
            count: 0,
            digest: Sha256::new(),
            path: path.to_path_buf(),
        }
    }

    fn finish(self) -> (u64, String) {
        (self.count, hex_digest(self.digest.finalize()))
    }

    fn limit_error(&self) -> io::Error {
        io::Error::other(format!(
            "expanded byte ceiling {} exceeded while writing {}",
            self.maximum_bytes,
            self.path.display()
        ))
    }
}

impl<W: Write> Write for EntryDigestWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let requested = u64::try_from(buffer.len()).unwrap_or(u64::MAX);
        let proposed = self
            .count
            .checked_add(requested)
            .ok_or_else(|| self.limit_error())?;
        if proposed > self.maximum_bytes {
            return Err(self.limit_error());
        }
        let count = self.inner.write(buffer)?;
        self.digest.update(&buffer[..count]);
        self.count = self
            .count
            .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
            .ok_or_else(|| self.limit_error())?;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A fixed, platform-independent `SplitMix64` stream.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn bounded(&mut self, upper_exclusive: u64) -> u64 {
        debug_assert!(upper_exclusive > 0);
        let threshold = upper_exclusive.wrapping_neg() % upper_exclusive;
        loop {
            let value = self.next_u64();
            if value >= threshold {
                return value % upper_exclusive;
            }
        }
    }

    fn event(&mut self, frequency: f64) -> bool {
        if frequency <= 0.0 {
            return false;
        }
        if frequency >= 1.0 {
            return true;
        }
        self.next_u64() < frequency_threshold(frequency)
    }
}

fn frequency_threshold(frequency: f64) -> u64 {
    let bits = frequency.to_bits();
    let exponent = u16::try_from((bits >> 52) & 0x7ff).unwrap_or(0);
    if exponent == 0 {
        return 0;
    }
    let significand = (1_u64 << 52) | (bits & ((1_u64 << 52) - 1));
    let shift = i32::from(exponent) - 1023 + 12;
    if shift >= 0 {
        significand
            .checked_shl(u32::try_from(shift).unwrap_or(u32::MAX))
            .unwrap_or(u64::MAX)
    } else {
        significand
            .checked_shr(shift.unsigned_abs())
            .unwrap_or_default()
    }
}

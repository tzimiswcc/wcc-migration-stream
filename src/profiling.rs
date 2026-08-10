//! Bounded, deterministic, object-neutral textual profiling.
//!
//! Profiling operates only on rows already accepted by intake. It describes
//! decoded text without inferring business meaning, data types, mappings, or
//! target-system compatibility.

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    KernelError, Result,
    state::{WorkspaceWriter, now_rfc3339, sqlite_integer},
};

/// Stable identity of the bounded profiler implementation.
pub const PROFILER_VERSION: &str = "1";
const PROFILER_NAME: &str = "migration-stream-text-profiler";

/// One exact duplicate-key calculation requested by policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DuplicateKeyPolicy {
    pub name: String,
    pub columns: Vec<String>,
}

/// Explicit bounds for textual profiling.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilingPolicy {
    pub schema_version: u32,
    pub columns_per_pass: usize,
    pub row_batch_size: u64,
    pub maximum_profiled_files: usize,
    pub maximum_profiled_columns: usize,
    pub maximum_exact_distinct_values_per_column: usize,
    pub maximum_exact_distinct_bytes_per_column: u64,
    pub maximum_exact_distinct_values_per_pass: usize,
    pub maximum_exact_distinct_bytes_per_pass: u64,
    pub maximum_samples_per_column: usize,
    pub maximum_sample_value_bytes: usize,
    pub oversized_field_bytes: usize,
    #[serde(default)]
    pub duplicate_keys: Vec<DuplicateKeyPolicy>,
}

impl ProfilingPolicy {
    /// Load and validate a profiling policy from JSON.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O, JSON, or policy error.
    pub fn from_path(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).map_err(|error| KernelError::io(path, error))?;
        let policy: Self = serde_json::from_slice(&bytes)?;
        policy.validate()?;
        Ok(policy)
    }

    /// Validate that every process-state and result-size bound is usable.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::PolicyInvalid`] for an invalid, inconsistent, or
    /// ambiguous bound. `maximum_samples_per_column` may be zero to retain no
    /// sample hashes or values.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(KernelError::PolicyInvalid(format!(
                "unsupported profiling policy schema version {}",
                self.schema_version
            )));
        }
        require_positive(self.columns_per_pass, "columns_per_pass")?;
        require_positive_u64(self.row_batch_size, "row_batch_size")?;
        require_positive(self.maximum_profiled_files, "maximum_profiled_files")?;
        require_positive(self.maximum_profiled_columns, "maximum_profiled_columns")?;
        require_positive(
            self.maximum_exact_distinct_values_per_column,
            "maximum_exact_distinct_values_per_column",
        )?;
        require_positive_u64(
            self.maximum_exact_distinct_bytes_per_column,
            "maximum_exact_distinct_bytes_per_column",
        )?;
        require_positive(
            self.maximum_exact_distinct_values_per_pass,
            "maximum_exact_distinct_values_per_pass",
        )?;
        require_positive_u64(
            self.maximum_exact_distinct_bytes_per_pass,
            "maximum_exact_distinct_bytes_per_pass",
        )?;
        require_positive(self.oversized_field_bytes, "oversized_field_bytes")?;
        if self.columns_per_pass > self.maximum_profiled_columns {
            return Err(KernelError::PolicyInvalid(
                "columns_per_pass exceeds maximum_profiled_columns".to_owned(),
            ));
        }
        if self.maximum_exact_distinct_values_per_column
            > self.maximum_exact_distinct_values_per_pass
        {
            return Err(KernelError::PolicyInvalid(
                "per-column distinct value bound exceeds the per-pass bound".to_owned(),
            ));
        }
        if self.maximum_exact_distinct_bytes_per_column > self.maximum_exact_distinct_bytes_per_pass
        {
            return Err(KernelError::PolicyInvalid(
                "per-column distinct byte bound exceeds the per-pass bound".to_owned(),
            ));
        }
        let mut names = HashSet::new();
        for key in &self.duplicate_keys {
            if key.name.trim().is_empty() {
                return Err(KernelError::PolicyInvalid(
                    "duplicate key names must not be empty".to_owned(),
                ));
            }
            if !names.insert(key.name.as_str()) {
                return Err(KernelError::PolicyInvalid(format!(
                    "duplicate key name {:?} is repeated",
                    key.name
                )));
            }
            if key.columns.is_empty() || key.columns.iter().any(String::is_empty) {
                return Err(KernelError::PolicyInvalid(format!(
                    "duplicate key {:?} requires nonempty column names",
                    key.name
                )));
            }
            let unique = key.columns.iter().collect::<HashSet<_>>();
            if unique.len() != key.columns.len() {
                return Err(KernelError::PolicyInvalid(format!(
                    "duplicate key {:?} repeats a column",
                    key.name
                )));
            }
        }
        Ok(())
    }
}

fn require_positive(value: usize, name: &str) -> Result<()> {
    if value == 0 {
        Err(KernelError::PolicyInvalid(format!(
            "{name} must be greater than zero"
        )))
    } else {
        Ok(())
    }
}

fn require_positive_u64(value: u64, name: &str) -> Result<()> {
    if value == 0 {
        Err(KernelError::PolicyInvalid(format!(
            "{name} must be greater than zero"
        )))
    } else {
        Ok(())
    }
}

/// Cooperative interruption controls used by the CLI and recovery tests.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilingControl {
    pub cancel_file: Option<PathBuf>,
    pub cancel_after_batches: Option<u64>,
}

/// Stable profiler identity embedded in every result.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProfilerIdentity {
    pub name: String,
    pub version: String,
}

/// Exactness state for one column's distinct-value calculation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DistinctMode {
    Exact,
    NotComputedDueToBound,
}

/// Bounded distinct-value evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DistinctProfile {
    pub mode: DistinctMode,
    pub count: Option<u64>,
}

/// One deterministic minimum-hash sample.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TextSample {
    pub sha256: String,
    pub utf8_bytes: u64,
    pub decoded_length: u64,
    /// Present only when the value is within the local sample byte cap.
    pub value: Option<String>,
}

/// Per-column textual metrics. Ordinals are zero-based and disambiguate names.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ColumnProfile {
    pub ordinal: u64,
    pub name: String,
    pub empty_count: u64,
    pub nonempty_count: u64,
    pub min_decoded_length: Option<u64>,
    pub max_decoded_length: Option<u64>,
    pub oversized_count: u64,
    pub distinct: DistinctProfile,
    pub samples: Vec<TextSample>,
}

/// Whether an exact configured duplicate key could be resolved in a file.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DuplicateKeyStatus {
    Exact,
    NotComputedMissingOrAmbiguousColumns,
}

/// Per-file result for one configured key.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DuplicateKeyProfile {
    pub name: String,
    pub columns: Vec<String>,
    pub status: DuplicateKeyStatus,
    pub distinct_key_count: Option<u64>,
    /// Number of rows beyond the first occurrence of each repeated key.
    pub duplicate_row_count: Option<u64>,
}

/// Deterministic result for one ingested CSV file.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FileProfile {
    pub path: String,
    pub row_count: u64,
    pub column_count: u64,
    pub duplicate_keys: Vec<DuplicateKeyProfile>,
    pub columns: Vec<ColumnProfile>,
}

/// Deterministic profiler output. It deliberately excludes run IDs, clocks,
/// host metrics, and any inferred business or target semantics. `job_id` is
/// retained as durable provenance but excluded from the canonical result hash.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProfilingResult {
    pub schema_version: u32,
    pub profiler: ProfilerIdentity,
    pub job_id: String,
    pub source_hash: String,
    pub policy_hash: String,
    pub file_count: u64,
    pub row_count: u64,
    pub column_count: u64,
    pub files: Vec<FileProfile>,
}

#[derive(Serialize)]
struct CanonicalProfilingResult<'a> {
    schema_version: u32,
    profiler: &'a ProfilerIdentity,
    source_hash: &'a str,
    policy_hash: &'a str,
    file_count: u64,
    row_count: u64,
    column_count: u64,
    files: &'a [FileProfile],
}

/// JSON-friendly durable profiling completion report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProfilingReport {
    pub run_id: String,
    pub identity_hash: String,
    pub result_hash: String,
    pub reused_complete: bool,
    pub elapsed_ms: u64,
    pub peak_rss_bytes: Option<u64>,
    pub result: ProfilingResult,
}

#[derive(Clone)]
struct FileDescriptor {
    path: String,
    header: Vec<String>,
    expected_rows: u64,
}

struct ProfileMetrics {
    started: Instant,
    peak_rss_bytes: Option<u64>,
}

impl ProfileMetrics {
    fn new() -> Self {
        let mut metrics = Self {
            started: Instant::now(),
            peak_rss_bytes: None,
        };
        metrics.sample_memory();
        metrics
    }

    fn sample_memory(&mut self) {
        if let Some(bytes) = app_memory_usage_fetcher::get_memory_usage_bytes() {
            self.peak_rss_bytes = Some(
                self.peak_rss_bytes
                    .map_or(bytes.get(), |current| current.max(bytes.get())),
            );
        }
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

struct PassBounds {
    distinct_values: usize,
    distinct_bytes: u64,
}

struct ColumnAccumulator {
    ordinal: u64,
    name: String,
    empty_count: u64,
    nonempty_count: u64,
    min_decoded_length: Option<u64>,
    max_decoded_length: Option<u64>,
    oversized_count: u64,
    distinct_values: Option<HashSet<String>>,
    distinct_bytes: u64,
    samples: BTreeMap<String, TextSample>,
}

impl ColumnAccumulator {
    fn new(ordinal: usize, name: String) -> Result<Self> {
        Ok(Self {
            ordinal: u64::try_from(ordinal).map_err(|_| {
                KernelError::Reconciliation("column ordinal does not fit u64".to_owned())
            })?,
            name,
            empty_count: 0,
            nonempty_count: 0,
            min_decoded_length: None,
            max_decoded_length: None,
            oversized_count: 0,
            distinct_values: Some(HashSet::new()),
            distinct_bytes: 0,
            samples: BTreeMap::new(),
        })
    }

    fn observe(&mut self, value: &str, policy: &ProfilingPolicy, pass: &mut PassBounds) {
        let utf8_bytes = u64::try_from(value.len()).unwrap_or(u64::MAX);
        let decoded_length = u64::try_from(value.chars().count()).unwrap_or(u64::MAX);
        if value.is_empty() {
            self.empty_count = self.empty_count.saturating_add(1);
        } else {
            self.nonempty_count = self.nonempty_count.saturating_add(1);
            self.min_decoded_length = Some(
                self.min_decoded_length
                    .map_or(decoded_length, |current| current.min(decoded_length)),
            );
            self.max_decoded_length = Some(
                self.max_decoded_length
                    .map_or(decoded_length, |current| current.max(decoded_length)),
            );
            self.observe_sample(value, utf8_bytes, decoded_length, policy);
        }
        if value.len() > policy.oversized_field_bytes {
            self.oversized_count = self.oversized_count.saturating_add(1);
        }
        self.observe_distinct(value, utf8_bytes, policy, pass);
    }

    fn observe_sample(
        &mut self,
        value: &str,
        utf8_bytes: u64,
        decoded_length: u64,
        policy: &ProfilingPolicy,
    ) {
        if policy.maximum_samples_per_column == 0 {
            return;
        }
        let hash = sha256_hex(value.as_bytes());
        if self.samples.contains_key(&hash) {
            return;
        }
        let should_insert = self.samples.len() < policy.maximum_samples_per_column
            || self
                .samples
                .last_key_value()
                .is_some_and(|(largest, _)| &hash < largest);
        if !should_insert {
            return;
        }
        let sample = TextSample {
            sha256: hash.clone(),
            utf8_bytes,
            decoded_length,
            value: (value.len() <= policy.maximum_sample_value_bytes).then(|| value.to_owned()),
        };
        self.samples.insert(hash, sample);
        if self.samples.len() > policy.maximum_samples_per_column {
            self.samples.pop_last();
        }
    }

    fn observe_distinct(
        &mut self,
        value: &str,
        utf8_bytes: u64,
        policy: &ProfilingPolicy,
        pass: &mut PassBounds,
    ) {
        let Some(values) = self.distinct_values.as_mut() else {
            return;
        };
        if values.contains(value) {
            return;
        }
        let next_column_values = values.len().saturating_add(1);
        let next_column_bytes = self.distinct_bytes.saturating_add(utf8_bytes);
        let next_pass_values = pass.distinct_values.saturating_add(1);
        let next_pass_bytes = pass.distinct_bytes.saturating_add(utf8_bytes);
        if next_column_values > policy.maximum_exact_distinct_values_per_column
            || next_column_bytes > policy.maximum_exact_distinct_bytes_per_column
            || next_pass_values > policy.maximum_exact_distinct_values_per_pass
            || next_pass_bytes > policy.maximum_exact_distinct_bytes_per_pass
        {
            pass.distinct_values = pass.distinct_values.saturating_sub(values.len());
            pass.distinct_bytes = pass.distinct_bytes.saturating_sub(self.distinct_bytes);
            self.distinct_values = None;
            self.distinct_bytes = 0;
            return;
        }
        values.insert(value.to_owned());
        self.distinct_bytes = next_column_bytes;
        pass.distinct_values = next_pass_values;
        pass.distinct_bytes = next_pass_bytes;
    }

    fn finish(self) -> ColumnProfile {
        let distinct = self.distinct_values.map_or(
            DistinctProfile {
                mode: DistinctMode::NotComputedDueToBound,
                count: None,
            },
            |values| DistinctProfile {
                mode: DistinctMode::Exact,
                count: Some(u64::try_from(values.len()).unwrap_or(u64::MAX)),
            },
        );
        ColumnProfile {
            ordinal: self.ordinal,
            name: self.name,
            empty_count: self.empty_count,
            nonempty_count: self.nonempty_count,
            min_decoded_length: self.min_decoded_length,
            max_decoded_length: self.max_decoded_length,
            oversized_count: self.oversized_count,
            distinct,
            samples: self.samples.into_values().collect(),
        }
    }
}

/// Profile a completed intake job using default (non-cancelling) controls.
///
/// # Errors
///
/// Returns a typed policy, state, cancellation, JSON, or `SQLite` error. A failed
/// or cancelled attempt is retained and can never expose a complete result.
pub fn run_profile(
    workspace: &Path,
    job_id: &str,
    policy: &ProfilingPolicy,
) -> Result<ProfilingReport> {
    run_profile_with_control(workspace, job_id, policy, &ProfilingControl::default())
}

/// Profile a completed intake job with cooperative interruption controls.
///
/// # Errors
///
/// Returns a typed policy, state, cancellation, JSON, or `SQLite` error. Only one
/// final transaction writes file/column results and changes the run to complete.
#[allow(clippy::too_many_lines)]
pub fn run_profile_with_control(
    workspace: &Path,
    job_id: &str,
    policy: &ProfilingPolicy,
    control: &ProfilingControl,
) -> Result<ProfilingReport> {
    policy.validate()?;
    let writer = WorkspaceWriter::open(workspace)?;
    let (source_hash, state): (Option<String>, String) = writer
        .connection
        .query_row(
            "SELECT source_hash, state FROM jobs WHERE id = ?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| {
            KernelError::Reconciliation(format!("intake job {job_id} does not exist"))
        })?;
    if state != "COMPLETE" {
        return Err(KernelError::Reconciliation(format!(
            "intake job {job_id} is {state}; only COMPLETE jobs may be profiled"
        )));
    }
    let source_hash = source_hash.ok_or_else(|| {
        KernelError::Reconciliation(format!("complete intake job {job_id} has no source hash"))
    })?;
    let policy_value = serde_json::to_value(policy)?;
    let policy_json = canonical_json_string(&policy_value)?;
    let policy_hash = sha256_hex(policy_json.as_bytes());
    let identity_hash = profile_identity_hash(&source_hash, &policy_value)?;

    if let Some(report) =
        reuse_complete(&writer, job_id, &source_hash, &policy_hash, &identity_hash)?
    {
        return Ok(report);
    }

    let run_id = Uuid::new_v4().to_string();
    create_run(
        &writer,
        &run_id,
        &identity_hash,
        job_id,
        &source_hash,
        &policy_json,
        &policy_hash,
    )?;
    let mut metrics = ProfileMetrics::new();
    let outcome = build_result(
        &writer,
        &run_id,
        job_id,
        &source_hash,
        &policy_hash,
        policy,
        control,
        &mut metrics,
    );
    metrics.sample_memory();
    let elapsed_ms = metrics.elapsed_ms();
    match outcome {
        Ok(result) => {
            let completion: Result<String> = (|| {
                let result_json = canonical_json_string(&serde_json::to_value(&result)?)?;
                let result_hash = canonical_result_hash(&result)?;
                persist_complete(
                    &writer,
                    &run_id,
                    &result,
                    &result_json,
                    &result_hash,
                    elapsed_ms,
                    metrics.peak_rss_bytes,
                )?;
                Ok(result_hash)
            })();
            match completion {
                Ok(result_hash) => Ok(ProfilingReport {
                    run_id,
                    identity_hash,
                    result_hash,
                    reused_complete: false,
                    elapsed_ms,
                    peak_rss_bytes: metrics.peak_rss_bytes,
                    result,
                }),
                Err(error) => {
                    mark_terminal(
                        &writer,
                        &run_id,
                        "FAILED",
                        error.code(),
                        &error.to_string(),
                        elapsed_ms,
                        metrics.peak_rss_bytes,
                    )?;
                    Err(error)
                }
            }
        }
        Err(error) => {
            let (state, code) = if matches!(error, KernelError::Cancelled(_)) {
                ("INTERRUPTED", "COOPERATIVE_CANCELLED")
            } else {
                ("FAILED", error.code())
            };
            mark_terminal(
                &writer,
                &run_id,
                state,
                code,
                &error.to_string(),
                elapsed_ms,
                metrics.peak_rss_bytes,
            )?;
            Err(error)
        }
    }
}

fn profile_identity_hash(source_hash: &str, policy: &Value) -> Result<String> {
    let value = serde_json::json!({
        "source_hash": source_hash,
        "policy": policy,
        "profiler_version": PROFILER_VERSION,
    });
    Ok(sha256_hex(canonical_json_string(&value)?.as_bytes()))
}

/// Hash only semantic profiling output. The intake job identifier remains in
/// the durable result JSON for provenance, but it is variable run metadata and
/// therefore cannot participate in the reproducibility contract.
pub(crate) fn canonical_result_hash(result: &ProfilingResult) -> Result<String> {
    let canonical = CanonicalProfilingResult {
        schema_version: result.schema_version,
        profiler: &result.profiler,
        source_hash: &result.source_hash,
        policy_hash: &result.policy_hash,
        file_count: result.file_count,
        row_count: result.row_count,
        column_count: result.column_count,
        files: &result.files,
    };
    canonical_result_hash_value(&serde_json::to_value(canonical)?)
}

/// Hash a persisted profiling result after removing only the top-level intake
/// job identifier. This value-level form lets integrity replay detect JSON
/// tampering independently from typed schema validation.
pub(crate) fn canonical_result_hash_value(result: &Value) -> Result<String> {
    let mut canonical = result.clone();
    let fields = canonical.as_object_mut().ok_or_else(|| {
        KernelError::Reconciliation("profiling result is not a JSON object".to_owned())
    })?;
    fields.remove("job_id");
    Ok(sha256_hex(canonical_json_string(&canonical)?.as_bytes()))
}

fn create_run(
    writer: &WorkspaceWriter,
    run_id: &str,
    identity_hash: &str,
    job_id: &str,
    source_hash: &str,
    policy_json: &str,
    policy_hash: &str,
) -> Result<()> {
    let transaction = writer.connection.unchecked_transaction()?;
    let now = now_rfc3339();
    transaction.execute(
        "INSERT INTO profiling_runs (
             id, identity_hash, job_id, source_hash, profiler_version,
             policy_json, policy_hash, state, created_at, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'RUNNING', ?8, ?8)",
        params![
            run_id,
            identity_hash,
            job_id,
            source_hash,
            PROFILER_VERSION,
            policy_json,
            policy_hash,
            now,
        ],
    )?;
    insert_profile_event(&transaction, run_id, "RUNNING", None, None, &now)?;
    transaction.commit()?;
    Ok(())
}

fn reuse_complete(
    writer: &WorkspaceWriter,
    job_id: &str,
    source_hash: &str,
    policy_hash: &str,
    identity_hash: &str,
) -> Result<Option<ProfilingReport>> {
    let existing: Option<(String, String, String, i64, Option<i64>)> = writer
        .connection
        .query_row(
            "SELECT id, result_json, result_hash, elapsed_ms, peak_rss_bytes
             FROM profiling_runs
             WHERE job_id = ?1 AND identity_hash = ?2 AND state = 'COMPLETE'
             ORDER BY completed_at, id LIMIT 1",
            params![job_id, identity_hash],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((run_id, result_json, result_hash, elapsed_ms, peak_rss_bytes)) = existing else {
        return Ok(None);
    };
    let result: ProfilingResult = serde_json::from_str(&result_json)?;
    if canonical_result_hash(&result)? != result_hash {
        return Err(KernelError::Reconciliation(format!(
            "complete profiling run {run_id} has a mismatched result hash"
        )));
    }
    if result.job_id != job_id
        || result.source_hash != source_hash
        || result.policy_hash != policy_hash
        || result.profiler.name != PROFILER_NAME
        || result.profiler.version != PROFILER_VERSION
    {
        return Err(KernelError::Reconciliation(format!(
            "complete profiling run {run_id} conflicts with its requested identity"
        )));
    }
    Ok(Some(ProfilingReport {
        run_id,
        identity_hash: identity_hash.to_owned(),
        result_hash,
        reused_complete: true,
        elapsed_ms: nonnegative_i64(elapsed_ms, "profiling elapsed milliseconds")?,
        peak_rss_bytes: peak_rss_bytes
            .map(|value| nonnegative_i64(value, "profiling peak RSS"))
            .transpose()?,
        result,
    }))
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn build_result(
    writer: &WorkspaceWriter,
    run_id: &str,
    job_id: &str,
    source_hash: &str,
    policy_hash: &str,
    policy: &ProfilingPolicy,
    control: &ProfilingControl,
    metrics: &mut ProfileMetrics,
) -> Result<ProfilingResult> {
    if cancellation_requested(control, 0) {
        return Err(KernelError::Cancelled(
            "profiling cancellation requested before the first batch".to_owned(),
        ));
    }
    writer
        .connection
        .pragma_update(None, "temp_store", "FILE")?;
    writer
        .connection
        .pragma_update(None, "cache_size", -2_048_i64)?;
    let descriptors = load_files(writer, job_id, policy)?;
    let crash_after = crash_after_batches();
    let mut committed_batches = 0_u64;
    let mut files = Vec::with_capacity(descriptors.len());
    let mut completed_rows = 0_u64;
    let mut completed_columns = 0_u64;

    for descriptor in descriptors {
        let mut observed_rows = None;
        let mut columns = Vec::with_capacity(descriptor.header.len());
        for start in (0..descriptor.header.len()).step_by(policy.columns_per_pass) {
            let end = start
                .saturating_add(policy.columns_per_pass)
                .min(descriptor.header.len());
            let mut accumulators = descriptor.header[start..end]
                .iter()
                .enumerate()
                .map(|(offset, name)| ColumnAccumulator::new(start + offset, name.clone()))
                .collect::<Result<Vec<_>>>()?;
            let mut pass_bounds = PassBounds {
                distinct_values: 0,
                distinct_bytes: 0,
            };
            let rows = scan_column_pass(
                writer,
                run_id,
                job_id,
                &descriptor,
                start,
                &mut accumulators,
                &mut pass_bounds,
                policy,
                control,
                metrics,
                crash_after,
                &mut committed_batches,
                u64::try_from(files.len()).unwrap_or(u64::MAX),
                completed_rows,
                completed_columns,
            )?;
            if observed_rows
                .replace(rows)
                .is_some_and(|prior| prior != rows)
            {
                return Err(KernelError::Reconciliation(format!(
                    "profiling row count changed between column passes for {:?}",
                    descriptor.path
                )));
            }
            columns.extend(accumulators.into_iter().map(ColumnAccumulator::finish));
            completed_columns =
                completed_columns.saturating_add(u64::try_from(end - start).unwrap_or(u64::MAX));
        }
        let row_count = observed_rows.unwrap_or(descriptor.expected_rows);
        if row_count != descriptor.expected_rows {
            return Err(KernelError::Reconciliation(format!(
                "profiling observed {row_count} rows for {:?}; durable file evidence reports {}",
                descriptor.path, descriptor.expected_rows
            )));
        }
        let duplicate_keys = profile_duplicate_keys(
            writer,
            run_id,
            job_id,
            &descriptor,
            policy,
            control,
            metrics,
            crash_after,
            &mut committed_batches,
            u64::try_from(files.len()).unwrap_or(u64::MAX),
            completed_rows.saturating_add(row_count),
            completed_columns,
        )?;
        completed_rows = completed_rows.saturating_add(row_count);
        files.push(FileProfile {
            path: descriptor.path,
            row_count,
            column_count: u64::try_from(columns.len()).unwrap_or(u64::MAX),
            duplicate_keys,
            columns,
        });
        update_progress(
            writer,
            run_id,
            u64::try_from(files.len()).unwrap_or(u64::MAX),
            completed_rows,
            completed_columns,
            metrics,
        )?;
    }
    Ok(ProfilingResult {
        schema_version: 1,
        profiler: ProfilerIdentity {
            name: PROFILER_NAME.to_owned(),
            version: PROFILER_VERSION.to_owned(),
        },
        job_id: job_id.to_owned(),
        source_hash: source_hash.to_owned(),
        policy_hash: policy_hash.to_owned(),
        file_count: u64::try_from(files.len()).unwrap_or(u64::MAX),
        row_count: completed_rows,
        column_count: completed_columns,
        files,
    })
}

fn load_files(
    writer: &WorkspaceWriter,
    job_id: &str,
    policy: &ProfilingPolicy,
) -> Result<Vec<FileDescriptor>> {
    let mut statement = writer.connection.prepare(
        "SELECT path, header_json, source_records, stored_records
         FROM csv_files WHERE job_id = ?1 ORDER BY path",
    )?;
    let mapped = statement.query_map([job_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut descriptors = Vec::new();
    let mut total_columns = 0_usize;
    for row in mapped {
        let (path, header_json, source_records, stored_records) = row?;
        if descriptors.len() >= policy.maximum_profiled_files {
            return Err(KernelError::PolicyInvalid(format!(
                "job {job_id} exceeds maximum_profiled_files {}",
                policy.maximum_profiled_files
            )));
        }
        let header_json = header_json.ok_or_else(|| {
            KernelError::Reconciliation(format!(
                "complete job {job_id} has no header evidence for {path:?}"
            ))
        })?;
        let header: Vec<String> = serde_json::from_str(&header_json)?;
        if header.is_empty() {
            return Err(KernelError::Reconciliation(format!(
                "complete job {job_id} has an empty header for {path:?}"
            )));
        }
        total_columns = total_columns.checked_add(header.len()).ok_or_else(|| {
            KernelError::PolicyInvalid("profiled column count overflow".to_owned())
        })?;
        if total_columns > policy.maximum_profiled_columns {
            return Err(KernelError::PolicyInvalid(format!(
                "job {job_id} exceeds maximum_profiled_columns {}",
                policy.maximum_profiled_columns
            )));
        }
        let source_records = nonnegative_i64(source_records, "CSV source records")?;
        let stored_records = nonnegative_i64(stored_records, "CSV stored records")?;
        if source_records != stored_records {
            return Err(KernelError::Reconciliation(format!(
                "complete job {job_id} has unreconciled CSV counters for {path:?}"
            )));
        }
        descriptors.push(FileDescriptor {
            path,
            header,
            expected_rows: stored_records,
        });
    }
    if descriptors.is_empty() {
        return Err(KernelError::Reconciliation(format!(
            "complete job {job_id} has no CSV files"
        )));
    }
    Ok(descriptors)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn scan_column_pass(
    writer: &WorkspaceWriter,
    run_id: &str,
    job_id: &str,
    descriptor: &FileDescriptor,
    start: usize,
    accumulators: &mut [ColumnAccumulator],
    pass_bounds: &mut PassBounds,
    policy: &ProfilingPolicy,
    control: &ProfilingControl,
    metrics: &mut ProfileMetrics,
    crash_after: Option<u64>,
    committed_batches: &mut u64,
    completed_files: u64,
    completed_rows: u64,
    completed_columns: u64,
) -> Result<u64> {
    let mut statement = writer.connection.prepare(
        "SELECT fields_json FROM source_records
         WHERE job_id = ?1 AND file_path = ?2 ORDER BY source_ordinal",
    )?;
    let mut rows = statement.query(params![job_id, descriptor.path])?;
    let mut row_count = 0_u64;
    while let Some(row) = rows.next()? {
        let fields_json: String = row.get(0)?;
        let fields: Vec<String> = serde_json::from_str(&fields_json)?;
        if fields.len() != descriptor.header.len() {
            return Err(KernelError::Reconciliation(format!(
                "stored row shape differs from header for {:?}",
                descriptor.path
            )));
        }
        for (offset, accumulator) in accumulators.iter_mut().enumerate() {
            accumulator.observe(&fields[start + offset], policy, pass_bounds);
        }
        row_count = row_count.saturating_add(1);
        if row_count.is_multiple_of(policy.row_batch_size) {
            *committed_batches = committed_batches.saturating_add(1);
            update_progress(
                writer,
                run_id,
                completed_files,
                completed_rows.saturating_add(row_count),
                completed_columns,
                metrics,
            )?;
            inject_hard_crash(crash_after, *committed_batches);
            check_cancellation(control, *committed_batches)?;
        }
    }
    if !row_count.is_multiple_of(policy.row_batch_size) {
        *committed_batches = committed_batches.saturating_add(1);
        update_progress(
            writer,
            run_id,
            completed_files,
            completed_rows.saturating_add(row_count),
            completed_columns,
            metrics,
        )?;
        inject_hard_crash(crash_after, *committed_batches);
        check_cancellation(control, *committed_batches)?;
    }
    Ok(row_count)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn profile_duplicate_keys(
    writer: &WorkspaceWriter,
    run_id: &str,
    job_id: &str,
    descriptor: &FileDescriptor,
    policy: &ProfilingPolicy,
    control: &ProfilingControl,
    metrics: &mut ProfileMetrics,
    crash_after: Option<u64>,
    committed_batches: &mut u64,
    completed_files: u64,
    completed_rows: u64,
    completed_columns: u64,
) -> Result<Vec<DuplicateKeyProfile>> {
    let mut profiles = Vec::with_capacity(policy.duplicate_keys.len());
    for configured in &policy.duplicate_keys {
        let indices = resolve_key_columns(&descriptor.header, &configured.columns);
        let Some(indices) = indices else {
            profiles.push(DuplicateKeyProfile {
                name: configured.name.clone(),
                columns: configured.columns.clone(),
                status: DuplicateKeyStatus::NotComputedMissingOrAmbiguousColumns,
                distinct_key_count: None,
                duplicate_row_count: None,
            });
            continue;
        };
        writer.connection.execute_batch(
            "DROP TABLE IF EXISTS temp.profiling_key_counts;
             CREATE TEMP TABLE profiling_key_counts (
                 key_json TEXT PRIMARY KEY NOT NULL,
                 occurrences INTEGER NOT NULL CHECK (occurrences > 0)
             ) WITHOUT ROWID;",
        )?;
        let mut statement = writer.connection.prepare(
            "SELECT fields_json FROM source_records
             WHERE job_id = ?1 AND file_path = ?2 ORDER BY source_ordinal",
        )?;
        let mut rows = statement.query(params![job_id, descriptor.path])?;
        let mut row_count = 0_u64;
        while let Some(row) = rows.next()? {
            let fields_json: String = row.get(0)?;
            let fields: Vec<String> = serde_json::from_str(&fields_json)?;
            if fields.len() != descriptor.header.len() {
                return Err(KernelError::Reconciliation(format!(
                    "stored row shape differs from header for {:?}",
                    descriptor.path
                )));
            }
            let key = indices
                .iter()
                .map(|index| fields[*index].as_str())
                .collect::<Vec<_>>();
            let key_json = serde_json::to_string(&key)?;
            writer.connection.execute(
                "INSERT INTO profiling_key_counts (key_json, occurrences) VALUES (?1, 1)
                 ON CONFLICT(key_json) DO UPDATE SET occurrences = occurrences + 1",
                [&key_json],
            )?;
            row_count = row_count.saturating_add(1);
            if row_count.is_multiple_of(policy.row_batch_size) {
                *committed_batches = committed_batches.saturating_add(1);
                update_progress(
                    writer,
                    run_id,
                    completed_files,
                    completed_rows,
                    completed_columns,
                    metrics,
                )?;
                inject_hard_crash(crash_after, *committed_batches);
                check_cancellation(control, *committed_batches)?;
            }
        }
        if !row_count.is_multiple_of(policy.row_batch_size) {
            *committed_batches = committed_batches.saturating_add(1);
            update_progress(
                writer,
                run_id,
                completed_files,
                completed_rows,
                completed_columns,
                metrics,
            )?;
            inject_hard_crash(crash_after, *committed_batches);
            check_cancellation(control, *committed_batches)?;
        }
        if row_count != descriptor.expected_rows {
            return Err(KernelError::Reconciliation(format!(
                "duplicate-key pass observed a different row count for {:?}",
                descriptor.path
            )));
        }
        let (distinct, duplicates): (i64, i64) = writer.connection.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN occurrences > 1 THEN occurrences - 1 ELSE 0 END), 0)
             FROM profiling_key_counts",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        profiles.push(DuplicateKeyProfile {
            name: configured.name.clone(),
            columns: configured.columns.clone(),
            status: DuplicateKeyStatus::Exact,
            distinct_key_count: Some(nonnegative_i64(distinct, "distinct key count")?),
            duplicate_row_count: Some(nonnegative_i64(duplicates, "duplicate row count")?),
        });
        writer
            .connection
            .execute_batch("DROP TABLE temp.profiling_key_counts;")?;
    }
    Ok(profiles)
}

fn resolve_key_columns(header: &[String], columns: &[String]) -> Option<Vec<usize>> {
    let mut indices = Vec::with_capacity(columns.len());
    for column in columns {
        let matches = header
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| (candidate == column).then_some(index))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return None;
        }
        indices.push(matches[0]);
    }
    Some(indices)
}

fn update_progress(
    writer: &WorkspaceWriter,
    run_id: &str,
    files: u64,
    rows: u64,
    columns: u64,
    metrics: &mut ProfileMetrics,
) -> Result<()> {
    metrics.sample_memory();
    let changed = writer.connection.execute(
        "UPDATE profiling_runs
         SET files_processed = MAX(files_processed, ?1),
             rows_processed = MAX(rows_processed, ?2),
             columns_processed = MAX(columns_processed, ?3),
             elapsed_ms = ?4, peak_rss_bytes = ?5, updated_at = ?6
         WHERE id = ?7 AND state = 'RUNNING'",
        params![
            sqlite_integer(files, "profiled files")?,
            sqlite_integer(rows, "profiled rows")?,
            sqlite_integer(columns, "profiled columns")?,
            sqlite_integer(metrics.elapsed_ms(), "profiling elapsed milliseconds")?,
            metrics
                .peak_rss_bytes
                .map(|value| sqlite_integer(value, "profiling peak RSS"))
                .transpose()?,
            now_rfc3339(),
            run_id,
        ],
    )?;
    require_one_run(changed, run_id, "record profiling progress")
}

#[allow(clippy::too_many_arguments)]
fn persist_complete(
    writer: &WorkspaceWriter,
    run_id: &str,
    result: &ProfilingResult,
    result_json: &str,
    result_hash: &str,
    elapsed_ms: u64,
    peak_rss_bytes: Option<u64>,
) -> Result<()> {
    let transaction = writer.connection.unchecked_transaction()?;
    for file in &result.files {
        transaction.execute(
            "INSERT INTO profiling_files (
                 run_id, path, row_count, column_count, duplicate_keys_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                run_id,
                file.path,
                sqlite_integer(file.row_count, "profile file rows")?,
                sqlite_integer(file.column_count, "profile file columns")?,
                canonical_json_string(&serde_json::to_value(&file.duplicate_keys)?)?,
            ],
        )?;
        for column in &file.columns {
            let distinct_mode = match column.distinct.mode {
                DistinctMode::Exact => "EXACT",
                DistinctMode::NotComputedDueToBound => "NOT_COMPUTED_DUE_TO_BOUND",
            };
            transaction.execute(
                "INSERT INTO profiling_columns (
                     run_id, path, column_ordinal, column_name, empty_count,
                     nonempty_count, min_decoded_length, max_decoded_length,
                     oversized_count, distinct_mode, distinct_count, samples_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    run_id,
                    file.path,
                    sqlite_integer(column.ordinal, "profile column ordinal")?,
                    column.name,
                    sqlite_integer(column.empty_count, "profile empty values")?,
                    sqlite_integer(column.nonempty_count, "profile nonempty values")?,
                    column
                        .min_decoded_length
                        .map(|value| sqlite_integer(value, "profile minimum decoded length"))
                        .transpose()?,
                    column
                        .max_decoded_length
                        .map(|value| sqlite_integer(value, "profile maximum decoded length"))
                        .transpose()?,
                    sqlite_integer(column.oversized_count, "profile oversized values")?,
                    distinct_mode,
                    column
                        .distinct
                        .count
                        .map(|value| sqlite_integer(value, "profile distinct values"))
                        .transpose()?,
                    canonical_json_string(&serde_json::to_value(&column.samples)?)?,
                ],
            )?;
        }
    }
    let now = now_rfc3339();
    let changed = transaction.execute(
        "UPDATE profiling_runs
         SET state = 'COMPLETE', result_json = ?1, result_hash = ?2,
             files_processed = ?3, rows_processed = ?4, columns_processed = ?5,
             elapsed_ms = ?6, peak_rss_bytes = ?7, updated_at = ?8, completed_at = ?8
         WHERE id = ?9 AND state = 'RUNNING'",
        params![
            result_json,
            result_hash,
            sqlite_integer(result.file_count, "profiled files")?,
            sqlite_integer(result.row_count, "profiled rows")?,
            sqlite_integer(result.column_count, "profiled columns")?,
            sqlite_integer(elapsed_ms, "profiling elapsed milliseconds")?,
            peak_rss_bytes
                .map(|value| sqlite_integer(value, "profiling peak RSS"))
                .transpose()?,
            now,
            run_id,
        ],
    )?;
    require_one_run(changed, run_id, "complete profiling run")?;
    insert_profile_event(&transaction, run_id, "COMPLETE", None, None, &now)?;
    transaction.commit()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn mark_terminal(
    writer: &WorkspaceWriter,
    run_id: &str,
    state: &str,
    error_code: &str,
    detail: &str,
    elapsed_ms: u64,
    peak_rss_bytes: Option<u64>,
) -> Result<()> {
    let transaction = writer.connection.unchecked_transaction()?;
    let now = now_rfc3339();
    let changed = transaction.execute(
        "UPDATE profiling_runs
         SET state = ?1, error_code = ?2, error_message = ?3,
             elapsed_ms = ?4, peak_rss_bytes = ?5, updated_at = ?6, completed_at = ?6
         WHERE id = ?7 AND state = 'RUNNING'",
        params![
            state,
            error_code,
            detail,
            sqlite_integer(elapsed_ms, "profiling elapsed milliseconds")?,
            peak_rss_bytes
                .map(|value| sqlite_integer(value, "profiling peak RSS"))
                .transpose()?,
            now,
            run_id,
        ],
    )?;
    require_one_run(changed, run_id, "finish profiling run")?;
    insert_profile_event(
        &transaction,
        run_id,
        state,
        Some(error_code),
        Some(detail),
        &now,
    )?;
    transaction.commit()?;
    Ok(())
}

fn insert_profile_event(
    transaction: &Transaction<'_>,
    run_id: &str,
    state: &str,
    error_code: Option<&str>,
    detail: Option<&str>,
    occurred_at: &str,
) -> Result<()> {
    transaction.execute(
        "INSERT INTO profiling_events (run_id, state, error_code, detail, occurred_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![run_id, state, error_code, detail, occurred_at],
    )?;
    Ok(())
}

fn require_one_run(changed: usize, run_id: &str, action: &str) -> Result<()> {
    if changed == 1 {
        Ok(())
    } else {
        Err(KernelError::Reconciliation(format!(
            "could not {action} {run_id}; changed {changed} rows"
        )))
    }
}

fn check_cancellation(control: &ProfilingControl, committed_batches: u64) -> Result<()> {
    if cancellation_requested(control, committed_batches) {
        Err(KernelError::Cancelled(format!(
            "profiling cancellation requested after {committed_batches} batches"
        )))
    } else {
        Ok(())
    }
}

fn cancellation_requested(control: &ProfilingControl, committed_batches: u64) -> bool {
    control
        .cancel_after_batches
        .is_some_and(|limit| committed_batches >= limit)
        || control
            .cancel_file
            .as_ref()
            .is_some_and(|path| path.exists())
}

fn crash_after_batches() -> Option<u64> {
    std::env::var("MIGRATION_STREAM_TEST_CRASH_PROFILE_AFTER_BATCHES")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
}

fn inject_hard_crash(configured: Option<u64>, committed_batches: u64) {
    if configured.is_some_and(|limit| committed_batches >= limit) {
        std::process::abort();
    }
}

fn canonical_json_string(value: &Value) -> Result<String> {
    serde_json::to_string(&canonicalize_json(value)).map_err(KernelError::from)
}

fn canonicalize_json(value: &Value) -> Value {
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

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn nonnegative_i64(value: i64, label: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        KernelError::Reconciliation(format!("{label} is negative in durable state: {value}"))
    })
}

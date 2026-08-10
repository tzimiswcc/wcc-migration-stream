//! Reproducible WP-002 benchmark orchestration and evidence.
//!
//! Each case executes generator, ingestion, profiling, and integrity commands in
//! fresh child processes. This keeps process-lifetime RSS observations isolated
//! by stage and lets the runner enforce wall-clock and RSS ceilings without
//! weakening the kernel's durable state transitions.

use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use chrono::{SecondsFormat, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{KernelError, Result, object_workspace::establish_for_local_automation};

const EVIDENCE_VERSION: &str = "migration-stream-benchmark-evidence/v1";
const MATRIX_VERSION: &str = "migration-stream-benchmark-matrix/v1";
const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
const CANCELLATION_GRACE: Duration = Duration::from_secs(5);

/// Version-controlled list of bounded benchmark cases.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkMatrix {
    pub schema_version: String,
    pub evidence_schema: PathBuf,
    pub cases: Vec<BenchmarkCase>,
}

/// One workload and its explicit host-safety envelope.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkCase {
    pub id: String,
    pub class: String,
    pub description: String,
    pub fixture_spec: PathBuf,
    pub intake_policy: PathBuf,
    pub profiling_policy: PathBuf,
    pub run_in_hosted_ci: bool,
    pub limits: BenchmarkLimits,
}

/// Refusal thresholds applied before and during a case.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkLimits {
    pub maximum_expanded_bytes: u64,
    pub minimum_free_space_bytes: u64,
    pub maximum_peak_rss_bytes: u64,
    pub maximum_wall_seconds: u64,
    pub require_external_rss: bool,
}

/// Exact source revision under measurement.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkSubject {
    pub git_commit: String,
    pub git_clean: bool,
}

/// Application and compiler identity.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkApplication {
    pub name: String,
    pub version: String,
    pub rustc_version: String,
}

/// Sanitized platform identity. `host_label` is supplied by the caller.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkPlatform {
    pub os: String,
    pub architecture: String,
    pub host_label: String,
}

/// Stable generated-input facts copied from the generator manifest.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkFixture {
    pub fixture_id: String,
    pub generator_version: String,
    pub seed: u64,
    pub parameters: Value,
    pub zip_bytes: u64,
    pub expanded_bytes: u64,
    pub file_count: u64,
    pub row_count: u64,
    pub column_count: u64,
    pub field_count: u64,
    pub zip_sha256: String,
}

/// Durable ingestion outcome and exact policy.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkIntake {
    pub policy: Value,
    pub elapsed_ms: u64,
    pub job_id: String,
    pub job_state: String,
    pub source_records: u64,
    pub stored_records: u64,
    pub pending_records: u64,
}

/// Durable completed profiling outcome and exact policy.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkProfiling {
    pub policy: Value,
    pub elapsed_ms: u64,
    pub run_id: String,
    pub state: String,
    pub result_sha256: String,
    pub reused_complete: bool,
}

/// End-to-end wall and sampled CPU time.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkTiming {
    pub end_to_end_elapsed_ms: u64,
    pub cpu_seconds: Option<f64>,
    pub cpu_method: String,
}

/// Maximum observed resident memory and its refusal ceiling.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkMemory {
    pub peak_rss_bytes: Option<u64>,
    pub method: String,
    pub configured_ceiling_bytes: u64,
}

/// Derived throughput, using completed ingestion elapsed time.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkThroughput {
    pub rows_per_second: f64,
    pub expanded_bytes_per_second: f64,
}

/// Final and live-sampled `SQLite` storage observations.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkStorage {
    pub sqlite_final_bytes: u64,
    pub wal_final_bytes: u64,
    pub shm_final_bytes: u64,
    pub sqlite_peak_bytes: Option<u64>,
    pub wal_peak_bytes: Option<u64>,
    pub shm_peak_bytes: Option<u64>,
    pub workspace_peak_bytes: Option<u64>,
    pub temporary_peak_bytes: Option<u64>,
    pub bytes_per_row: f64,
    pub bytes_per_field: f64,
    pub allocations_bytes: BTreeMap<String, u64>,
}

/// Closing reconciliation and replay assertions.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkOutcome {
    pub reconciliation_ok: bool,
    pub integrity_ok: bool,
    pub integrity_elapsed_ms: u64,
    pub integrity_checked_jobs: u64,
    pub integrity_checked_packages: u64,
    pub integrity_checked_profiling_runs: u64,
}

/// Machine-readable evidence validated before publication.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkEvidence {
    pub schema_version: String,
    pub case_id: String,
    pub recorded_at_utc: String,
    pub subject: BenchmarkSubject,
    pub application: BenchmarkApplication,
    pub platform: BenchmarkPlatform,
    pub limits: BenchmarkLimits,
    pub fixture: BenchmarkFixture,
    pub intake: BenchmarkIntake,
    pub profiling: BenchmarkProfiling,
    pub timing: BenchmarkTiming,
    pub memory: BenchmarkMemory,
    pub throughput: BenchmarkThroughput,
    pub storage: BenchmarkStorage,
    pub outcome: BenchmarkOutcome,
    pub warnings: Vec<String>,
    pub limitations: Vec<String>,
}

/// Paths plus the evidence returned by a successful case.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BenchmarkRunReport {
    pub output_directory: PathBuf,
    pub evidence_path: PathBuf,
    pub summary_path: PathBuf,
    pub evidence: BenchmarkEvidence,
}

#[derive(Default)]
#[allow(clippy::struct_field_names)]
struct StorageObservation {
    sqlite_peak: Option<u64>,
    wal_peak: Option<u64>,
    shm_peak: Option<u64>,
    workspace_peak: Option<u64>,
}

struct ChildObservation {
    result: Value,
    elapsed_ms: u64,
    peak_rss_bytes: Option<u64>,
    cpu_seconds: Option<f64>,
    external_rss_observed: bool,
}

struct ChildSpec<'a> {
    stage: &'a str,
    arguments: Vec<String>,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    cancellation_path: Option<PathBuf>,
    workspace: Option<&'a Path>,
}

/// Load and structurally validate a benchmark matrix.
///
/// # Errors
///
/// Returns an I/O, JSON, or validation error for an unusable matrix.
pub fn load_matrix(path: &Path) -> Result<BenchmarkMatrix> {
    let bytes = fs::read(path).map_err(|error| KernelError::io(path, error))?;
    let matrix: BenchmarkMatrix = serde_json::from_slice(&bytes)?;
    if matrix.schema_version != MATRIX_VERSION || matrix.cases.is_empty() {
        return Err(KernelError::Reconciliation(format!(
            "benchmark matrix must use {MATRIX_VERSION:?} and contain at least one case"
        )));
    }
    let mut identifiers = HashSet::new();
    for case in &matrix.cases {
        validate_token(&case.id, "benchmark case ID")?;
        if !identifiers.insert(&case.id) {
            return Err(KernelError::Reconciliation(format!(
                "benchmark matrix repeats case ID {:?}",
                case.id
            )));
        }
        validate_case(case)?;
    }
    Ok(matrix)
}

/// Execute one matrix case in an isolated, newly-created output directory.
///
/// # Errors
///
/// Refuses unknown cases, unsafe host labels, existing output paths, inadequate
/// free space, unavailable required RSS measurement, exceeded limits, failed
/// child stages, invalid evidence, or non-reconciled outcomes.
#[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
pub fn run_benchmark_case(
    matrix_path: &Path,
    case_id: &str,
    output_directory: &Path,
    host_label: &str,
) -> Result<BenchmarkRunReport> {
    validate_token(host_label, "sanitized host label")?;
    let matrix = load_matrix(matrix_path)?;
    let case = matrix
        .cases
        .iter()
        .find(|candidate| candidate.id == case_id)
        .ok_or_else(|| {
            KernelError::Reconciliation(format!(
                "benchmark case {case_id:?} is not present in {}",
                matrix_path.display()
            ))
        })?;
    let matrix_root = matrix_path.parent().unwrap_or_else(|| Path::new("."));
    let fixture_spec = matrix_root.join(&case.fixture_spec);
    let intake_policy_path = matrix_root.join(&case.intake_policy);
    let profiling_policy_path = matrix_root.join(&case.profiling_policy);
    let evidence_schema_path = matrix_root.join(&matrix.evidence_schema);
    ensure_regular_file(&fixture_spec)?;
    ensure_regular_file(&intake_policy_path)?;
    ensure_regular_file(&profiling_policy_path)?;
    ensure_regular_file(&evidence_schema_path)?;

    let output_parent = output_directory.parent().unwrap_or_else(|| Path::new("."));
    let available = fs2::available_space(output_parent)
        .map_err(|error| KernelError::io(output_parent, error))?;
    if available < case.limits.minimum_free_space_bytes {
        return Err(KernelError::InsufficientSpace {
            required: case.limits.minimum_free_space_bytes,
            available,
        });
    }
    fs::create_dir(output_directory).map_err(|error| KernelError::io(output_directory, error))?;
    let fixture_directory = output_directory.join("fixture");
    fs::create_dir(&fixture_directory)
        .map_err(|error| KernelError::io(&fixture_directory, error))?;
    let workspace = output_directory.join("workspace");
    fs::create_dir(&workspace).map_err(|error| KernelError::io(&workspace, error))?;
    let executable =
        std::env::current_exe().map_err(|error| KernelError::io("current_exe", error))?;
    let overall_started = Instant::now();
    establish_for_local_automation(
        &workspace,
        &format!("Synthetic benchmark {}", case.id),
        &format!("Synthetic {} benchmark", case.class),
        "Migration Stream benchmark runner",
    )?;
    let mut storage_observation = StorageObservation::default();

    let fixture_child = run_child(
        &executable,
        &ChildSpec {
            stage: "fixture",
            arguments: vec![
                "fixture".to_owned(),
                "generate".to_owned(),
                "--spec".to_owned(),
                fixture_spec.to_string_lossy().into_owned(),
                "--output-directory".to_owned(),
                fixture_directory.to_string_lossy().into_owned(),
            ],
            stdout_path: output_directory.join("fixture.stdout.json"),
            stderr_path: output_directory.join("fixture.stderr.txt"),
            cancellation_path: None,
            workspace: None,
        },
        &case.limits,
        &mut storage_observation,
    )?;
    let manifest_path = fixture_child
        .result
        .get("manifest_path")
        .and_then(Value::as_str)
        .map_or_else(
            || {
                fixture_directory.join(format!(
                    "{}.manifest.json",
                    fixture_name(&fixture_spec).unwrap_or_else(|_| case.id.clone())
                ))
            },
            PathBuf::from,
        );
    let manifest = read_json(&manifest_path)?;
    let fixture = fixture_evidence(&manifest)?;
    if fixture.expanded_bytes > case.limits.maximum_expanded_bytes {
        return Err(KernelError::Reconciliation(format!(
            "fixture expanded size {} exceeds case ceiling {}",
            fixture.expanded_bytes, case.limits.maximum_expanded_bytes
        )));
    }
    let archive_path =
        fixture_directory.join(json_string(&manifest, &["valid_archive", "file_name"])?);

    let ingest_cancel = output_directory.join("ingest.cancel");
    let ingest_child = run_child(
        &executable,
        &ChildSpec {
            stage: "ingest",
            arguments: vec![
                "ingest".to_owned(),
                "--source".to_owned(),
                archive_path.to_string_lossy().into_owned(),
                "--workspace".to_owned(),
                workspace.to_string_lossy().into_owned(),
                "--policy".to_owned(),
                intake_policy_path.to_string_lossy().into_owned(),
                "--cancel-file".to_owned(),
                ingest_cancel.to_string_lossy().into_owned(),
            ],
            stdout_path: output_directory.join("ingest.stdout.json"),
            stderr_path: output_directory.join("ingest.stderr.txt"),
            cancellation_path: Some(ingest_cancel),
            workspace: Some(&workspace),
        },
        &case.limits,
        &mut storage_observation,
    )?;
    let job_id = json_string(&ingest_child.result, &["job_id"])?;

    let profile_cancel = output_directory.join("profile.cancel");
    let profile_child = run_child(
        &executable,
        &ChildSpec {
            stage: "profiling",
            arguments: vec![
                "profile".to_owned(),
                "run".to_owned(),
                "--workspace".to_owned(),
                workspace.to_string_lossy().into_owned(),
                "--job".to_owned(),
                job_id.clone(),
                "--policy".to_owned(),
                profiling_policy_path.to_string_lossy().into_owned(),
                "--cancel-file".to_owned(),
                profile_cancel.to_string_lossy().into_owned(),
            ],
            stdout_path: output_directory.join("profiling.stdout.json"),
            stderr_path: output_directory.join("profiling.stderr.txt"),
            cancellation_path: Some(profile_cancel),
            workspace: Some(&workspace),
        },
        &case.limits,
        &mut storage_observation,
    )?;

    let integrity_child = run_child(
        &executable,
        &ChildSpec {
            stage: "integrity",
            arguments: vec![
                "integrity".to_owned(),
                "--workspace".to_owned(),
                workspace.to_string_lossy().into_owned(),
            ],
            stdout_path: output_directory.join("integrity.stdout.json"),
            stderr_path: output_directory.join("integrity.stderr.txt"),
            cancellation_path: None,
            workspace: Some(&workspace),
        },
        &case.limits,
        &mut storage_observation,
    )?;

    let intake_policy = read_json(&intake_policy_path)?;
    let profiling_policy = read_json(&profiling_policy_path)?;
    let subject = git_subject()?;
    let mut warnings = Vec::new();
    if !subject.git_clean {
        warnings.push("subject worktree was dirty when the benchmark began".to_owned());
    }
    let mut limitations = vec![
        "RSS combines stage-local process-lifetime observations with 100 ms external samples; short-lived peaks may only be represented by the internal observation".to_owned(),
        "bytes-per-row and bytes-per-field divide the complete SQLite main file, including schema and non-row evidence".to_owned(),
        "peak workspace bytes are sampled at 100 ms; SQLite temporary files outside the workspace are not measured".to_owned(),
    ];

    let child_cpu = [
        fixture_child.cpu_seconds,
        ingest_child.cpu_seconds,
        profile_child.cpu_seconds,
        integrity_child.cpu_seconds,
    ];
    let cpu_seconds = child_cpu
        .iter()
        .copied()
        .try_fold(0.0_f64, |total, value| value.map(|seconds| total + seconds))
        .filter(|seconds| *seconds > 0.0);
    let cpu_method = if cpu_seconds.is_some() {
        "sum of final ps process-time samples for the four isolated child stages".to_owned()
    } else {
        limitations.push("reliable child CPU-time sampling was unavailable".to_owned());
        "unavailable".to_owned()
    };
    let external_rss_observed =
        ingest_child.external_rss_observed && profile_child.external_rss_observed;
    if case.limits.require_external_rss && !external_rss_observed {
        return Err(KernelError::Reconciliation(
            "case requires external RSS observation, but ps sampling was unavailable".to_owned(),
        ));
    }
    let peak_rss_bytes = max_options([
        fixture_child.peak_rss_bytes,
        ingest_child.peak_rss_bytes,
        profile_child.peak_rss_bytes,
        integrity_child.peak_rss_bytes,
        optional_json_u64(&ingest_child.result, &["peak_rss_bytes"]),
        optional_json_u64(&profile_child.result, &["peak_rss_bytes"]),
    ]);
    if peak_rss_bytes.is_some_and(|rss| rss > case.limits.maximum_peak_rss_bytes) {
        return Err(KernelError::Reconciliation(format!(
            "observed peak RSS {} exceeds case ceiling {}",
            peak_rss_bytes.unwrap_or(u64::MAX),
            case.limits.maximum_peak_rss_bytes
        )));
    }

    merge_persisted_storage_peaks(&ingest_child.result, &mut storage_observation);
    sample_workspace_storage(&workspace, &mut storage_observation)?;
    let sqlite_path = workspace.join("state.sqlite3");
    let sqlite_final = file_size_or_zero(&sqlite_path)?;
    let wal_final = file_size_or_zero(&workspace.join("state.sqlite3-wal"))?;
    let shm_final = file_size_or_zero(&workspace.join("state.sqlite3-shm"))?;
    let allocations = sqlite_allocations(&sqlite_path).unwrap_or_else(|error| {
        limitations.push(format!(
            "SQLite dbstat allocation data unavailable: {error}"
        ));
        BTreeMap::new()
    });

    let source_records = json_u64(&ingest_child.result, &["source_records"])?;
    let stored_records = json_u64(&ingest_child.result, &["stored_records"])?;
    let pending_records = json_u64(&ingest_child.result, &["pending_records"])?;
    let ingest_elapsed_ms = json_u64(&ingest_child.result, &["elapsed_ms"])?;
    let profile_elapsed_ms =
        json_u64(&profile_child.result, &["elapsed_ms"]).unwrap_or(profile_child.elapsed_ms);
    let duration_seconds = (ingest_elapsed_ms.max(1) as f64) / 1_000.0;
    let reconciliation_ok = source_records == stored_records && pending_records == 0;
    let integrity_ok = json_bool(&integrity_child.result, &["ok"])?;
    if !reconciliation_ok || !integrity_ok {
        return Err(KernelError::Reconciliation(
            "benchmark closing reconciliation or integrity assertion failed".to_owned(),
        ));
    }

    let evidence = BenchmarkEvidence {
        schema_version: EVIDENCE_VERSION.to_owned(),
        case_id: case.id.clone(),
        recorded_at_utc: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        subject,
        application: BenchmarkApplication {
            name: env!("CARGO_PKG_NAME").to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            rustc_version: command_text("rustc", &["--version"])
                .unwrap_or_else(|| "unavailable".to_owned()),
        },
        platform: BenchmarkPlatform {
            os: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            host_label: host_label.to_owned(),
        },
        limits: case.limits.clone(),
        throughput: BenchmarkThroughput {
            rows_per_second: (source_records as f64) / duration_seconds,
            expanded_bytes_per_second: (fixture.expanded_bytes as f64) / duration_seconds,
        },
        intake: BenchmarkIntake {
            policy: intake_policy,
            elapsed_ms: ingest_elapsed_ms,
            job_id,
            job_state: "COMPLETE".to_owned(),
            source_records,
            stored_records,
            pending_records,
        },
        profiling: BenchmarkProfiling {
            policy: profiling_policy,
            elapsed_ms: profile_elapsed_ms,
            run_id: json_string(&profile_child.result, &["run_id"])?,
            state: "COMPLETE".to_owned(),
            result_sha256: profile_result_hash(&profile_child.result)?,
            reused_complete: optional_json_bool(&profile_child.result, &["reused_complete"])
                .unwrap_or(false),
        },
        timing: BenchmarkTiming {
            end_to_end_elapsed_ms: millis(overall_started.elapsed()),
            cpu_seconds,
            cpu_method,
        },
        memory: BenchmarkMemory {
            peak_rss_bytes,
            method: if external_rss_observed {
                "maximum of internal process-lifetime RSS and 100 ms ps RSS samples across isolated stages".to_owned()
            } else {
                "internal process-lifetime RSS where reported; external ps sampling incomplete"
                    .to_owned()
            },
            configured_ceiling_bytes: case.limits.maximum_peak_rss_bytes,
        },
        storage: BenchmarkStorage {
            sqlite_final_bytes: sqlite_final,
            wal_final_bytes: wal_final,
            shm_final_bytes: shm_final,
            sqlite_peak_bytes: storage_observation.sqlite_peak,
            wal_peak_bytes: storage_observation.wal_peak,
            shm_peak_bytes: storage_observation.shm_peak,
            workspace_peak_bytes: storage_observation.workspace_peak,
            temporary_peak_bytes: None,
            bytes_per_row: ratio(sqlite_final, source_records),
            bytes_per_field: ratio(sqlite_final, fixture.field_count),
            allocations_bytes: allocations,
        },
        outcome: BenchmarkOutcome {
            reconciliation_ok,
            integrity_ok,
            integrity_elapsed_ms: integrity_child.elapsed_ms,
            integrity_checked_jobs: json_u64(&integrity_child.result, &["checked_jobs"])?,
            integrity_checked_packages: json_u64(&integrity_child.result, &["checked_packages"])?,
            integrity_checked_profiling_runs: optional_json_u64(
                &integrity_child.result,
                &["checked_profiling_runs"],
            )
            .unwrap_or(0),
        },
        fixture,
        warnings,
        limitations,
    };
    validate_evidence(&evidence_schema_path, &evidence)?;
    let evidence_path = output_directory.join("evidence.json");
    let summary_path = output_directory.join("summary.md");
    write_new_json(&evidence_path, &evidence)?;
    write_new_text(&summary_path, &render_summary(case, &evidence))?;
    Ok(BenchmarkRunReport {
        output_directory: output_directory.to_path_buf(),
        evidence_path,
        summary_path,
        evidence,
    })
}

fn validate_case(case: &BenchmarkCase) -> Result<()> {
    if case.class.trim().is_empty()
        || case.description.trim().is_empty()
        || case.limits.maximum_expanded_bytes == 0
        || case.limits.minimum_free_space_bytes == 0
        || case.limits.maximum_peak_rss_bytes == 0
        || case.limits.maximum_wall_seconds == 0
    {
        return Err(KernelError::Reconciliation(format!(
            "benchmark case {:?} has an empty description/class or a zero safety limit",
            case.id
        )));
    }
    let minimum_guard = case
        .limits
        .maximum_expanded_bytes
        .checked_mul(6)
        .ok_or_else(|| {
            KernelError::Reconciliation(format!(
                "benchmark case {:?} expanded-byte guard overflows",
                case.id
            ))
        })?;
    if case.limits.minimum_free_space_bytes < minimum_guard {
        return Err(KernelError::Reconciliation(format!(
            "benchmark case {:?} must reserve at least six times its expanded-byte ceiling",
            case.id
        )));
    }
    Ok(())
}

fn validate_token(value: &str, label: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= 80
        && value.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' | b'0'..=b'9' => true,
            b'.' | b'_' | b'-' => index > 0,
            _ => false,
        });
    if valid {
        Ok(())
    } else {
        Err(KernelError::Reconciliation(format!(
            "{label} must be a lowercase, non-sensitive token of at most 80 characters"
        )))
    }
}

fn ensure_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(KernelError::SourceNotRegular(path.to_path_buf()))
    }
}

fn fixture_name(spec_path: &Path) -> Result<String> {
    let value = read_json(spec_path)?;
    json_string(&value, &["fixture_name"])
}

fn read_json(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).map_err(|error| KernelError::io(path, error))?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}

fn fixture_evidence(manifest: &Value) -> Result<BenchmarkFixture> {
    let parameters = manifest
        .get("spec")
        .cloned()
        .ok_or_else(|| missing_json("spec"))?;
    let files = manifest
        .pointer("/valid_archive/files")
        .and_then(Value::as_array)
        .ok_or_else(|| missing_json("valid_archive.files"))?;
    let row_count = files.iter().try_fold(0_u64, |total, file| {
        let rows = json_u64(file, &["rows"])?;
        total.checked_add(rows).ok_or_else(|| {
            KernelError::Reconciliation("fixture row count overflows u64".to_owned())
        })
    })?;
    let file_count = u64::try_from(files.len())
        .map_err(|_| KernelError::Reconciliation("fixture file count overflows u64".to_owned()))?;
    let column_count = json_u64(&parameters, &["columns"])?;
    let field_count = row_count.checked_mul(column_count).ok_or_else(|| {
        KernelError::Reconciliation("fixture field count overflows u64".to_owned())
    })?;
    Ok(BenchmarkFixture {
        fixture_id: json_string(manifest, &["fixture_identity"])?,
        generator_version: json_string(manifest, &["generator_version"])?,
        seed: json_u64(manifest, &["seed"])?,
        parameters,
        zip_bytes: json_u64(manifest, &["valid_archive", "size_bytes"])?,
        expanded_bytes: json_u64(manifest, &["total_expanded_bytes"])?,
        file_count,
        row_count,
        column_count,
        field_count,
        zip_sha256: json_string(manifest, &["valid_archive", "sha256"])?,
    })
}

fn run_child(
    executable: &Path,
    spec: &ChildSpec<'_>,
    limits: &BenchmarkLimits,
    storage: &mut StorageObservation,
) -> Result<ChildObservation> {
    let stdout = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&spec.stdout_path)
        .map_err(|error| KernelError::io(&spec.stdout_path, error))?;
    let stderr = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&spec.stderr_path)
        .map_err(|error| KernelError::io(&spec.stderr_path, error))?;
    let mut child = Command::new(executable)
        .args(&spec.arguments)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| KernelError::io(executable, error))?;
    let started = Instant::now();
    let wall_limit = Duration::from_secs(limits.maximum_wall_seconds);
    let mut peak_rss_bytes = None;
    let mut cpu_seconds = None;
    let mut external_rss_observed = false;
    let mut cancellation_requested_at = None;
    let mut guard_failure = None;

    let status = loop {
        if let Some(sample) = sample_process(child.id()) {
            external_rss_observed = true;
            peak_rss_bytes = Some(peak_rss_bytes.map_or(sample.rss_bytes, |current: u64| {
                current.max(sample.rss_bytes)
            }));
            cpu_seconds = Some(cpu_seconds.map_or(sample.cpu_seconds, |current: f64| {
                current.max(sample.cpu_seconds)
            }));
            if sample.rss_bytes > limits.maximum_peak_rss_bytes && guard_failure.is_none() {
                guard_failure = Some(format!(
                    "{} exceeded RSS ceiling: {} > {} bytes",
                    spec.stage, sample.rss_bytes, limits.maximum_peak_rss_bytes
                ));
            }
        }
        if let Some(workspace) = spec.workspace {
            sample_workspace_storage(workspace, storage)?;
        }
        if started.elapsed() > wall_limit && guard_failure.is_none() {
            guard_failure = Some(format!(
                "{} exceeded wall ceiling of {} seconds",
                spec.stage, limits.maximum_wall_seconds
            ));
        }
        if guard_failure.is_some() && cancellation_requested_at.is_none() {
            if let Some(path) = &spec.cancellation_path {
                write_cancellation_token(path)?;
                cancellation_requested_at = Some(Instant::now());
            } else {
                child
                    .kill()
                    .map_err(|error| KernelError::io(executable, error))?;
                cancellation_requested_at = Some(Instant::now());
            }
        }
        if cancellation_requested_at.is_some_and(|time| time.elapsed() >= CANCELLATION_GRACE) {
            let _ = child.kill();
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| KernelError::io(executable, error))?
        {
            break status;
        }
        thread::sleep(SAMPLE_INTERVAL);
    };
    if let Some(workspace) = spec.workspace {
        sample_workspace_storage(workspace, storage)?;
    }
    let elapsed_ms = millis(started.elapsed());
    if let Some(failure) = guard_failure {
        return Err(KernelError::Reconciliation(failure));
    }
    child_result(spec.stage, status, &spec.stdout_path, &spec.stderr_path).map(|result| {
        ChildObservation {
            result,
            elapsed_ms,
            peak_rss_bytes,
            cpu_seconds,
            external_rss_observed,
        }
    })
}

fn child_result(
    stage: &str,
    status: ExitStatus,
    stdout_path: &Path,
    stderr_path: &Path,
) -> Result<Value> {
    if !status.success() {
        let detail = bounded_file_text(stderr_path, 4_096)?;
        return Err(KernelError::Reconciliation(format!(
            "benchmark {stage} child failed with {status}: {detail}"
        )));
    }
    let envelope = read_json(stdout_path)?;
    if envelope.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(KernelError::Reconciliation(format!(
            "benchmark {stage} child returned a non-success JSON envelope"
        )));
    }
    envelope
        .get("result")
        .cloned()
        .ok_or_else(|| missing_json("result"))
}

fn bounded_file_text(path: &Path, maximum_chars: usize) -> Result<String> {
    let mut file = File::open(path).map_err(|error| KernelError::io(path, error))?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| KernelError::io(path, error))?;
    Ok(text.chars().take(maximum_chars).collect())
}

struct ProcessSample {
    rss_bytes: u64,
    cpu_seconds: f64,
}

fn sample_process(pid: u32) -> Option<ProcessSample> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-o", "time=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let mut fields = text.split_whitespace();
    let rss_kib = fields.next()?.parse::<u64>().ok()?;
    let cpu_seconds = parse_process_time(fields.next()?)?;
    Some(ProcessSample {
        rss_bytes: rss_kib.saturating_mul(1_024),
        cpu_seconds,
    })
}

#[allow(clippy::cast_precision_loss)]
fn parse_process_time(value: &str) -> Option<f64> {
    let (days, clock) = value
        .split_once('-')
        .map_or((0_u64, value), |(days, clock)| {
            (days.parse::<u64>().ok().unwrap_or(0), clock)
        });
    let fields = clock.split(':').collect::<Vec<_>>();
    let (hours, minutes, seconds): (u64, u64, f64) = match fields.as_slice() {
        [minutes, seconds] => (0_u64, minutes.parse().ok()?, seconds.parse::<f64>().ok()?),
        [hours, minutes, seconds] => (
            hours.parse().ok()?,
            minutes.parse().ok()?,
            seconds.parse::<f64>().ok()?,
        ),
        _ => return None,
    };
    Some(
        (days.saturating_mul(86_400) + hours.saturating_mul(3_600) + minutes.saturating_mul(60))
            as f64
            + seconds,
    )
}

fn write_cancellation_token(path: &Path) -> Result<()> {
    let result = OpenOptions::new().create_new(true).write(true).open(path);
    match result {
        Ok(mut file) => file
            .write_all(b"cancel\n")
            .map_err(|error| KernelError::io(path, error)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(KernelError::io(path, error)),
    }
}

fn sample_workspace_storage(workspace: &Path, observation: &mut StorageObservation) -> Result<()> {
    if !workspace.exists() {
        return Ok(());
    }
    let sqlite = file_size_or_zero(&workspace.join("state.sqlite3"))?;
    let wal = file_size_or_zero(&workspace.join("state.sqlite3-wal"))?;
    let shm = file_size_or_zero(&workspace.join("state.sqlite3-shm"))?;
    let total = directory_size(workspace)?;
    observation.sqlite_peak = max_option(observation.sqlite_peak, Some(sqlite));
    observation.wal_peak = max_option(observation.wal_peak, Some(wal));
    observation.shm_peak = max_option(observation.shm_peak, Some(shm));
    observation.workspace_peak = max_option(observation.workspace_peak, Some(total));
    Ok(())
}

fn directory_size(path: &Path) -> Result<u64> {
    let mut total = 0_u64;
    let entries = fs::read_dir(path).map_err(|error| KernelError::io(path, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| KernelError::io(path, error))?;
        let child = entry.path();
        let metadata =
            fs::symlink_metadata(&child).map_err(|error| KernelError::io(&child, error))?;
        if metadata.file_type().is_file() {
            total = total.saturating_add(metadata.len());
        } else if metadata.file_type().is_dir() {
            total = total.saturating_add(directory_size(&child)?);
        }
    }
    Ok(total)
}

fn file_size_or_zero(path: &Path) -> Result<u64> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(metadata.len()),
        Ok(_) => Err(KernelError::Reconciliation(format!(
            "storage evidence path {} is not a regular file",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(KernelError::io(path, error)),
    }
}

fn merge_persisted_storage_peaks(result: &Value, observation: &mut StorageObservation) {
    observation.sqlite_peak = max_option(
        observation.sqlite_peak,
        optional_json_u64(result, &["peak_sqlite_bytes"])
            .or_else(|| optional_json_u64(result, &["peak_database_bytes"])),
    );
    observation.wal_peak = max_option(
        observation.wal_peak,
        optional_json_u64(result, &["peak_wal_bytes"]),
    );
    observation.shm_peak = max_option(
        observation.shm_peak,
        optional_json_u64(result, &["peak_shm_bytes"]),
    );
}

fn sqlite_allocations(database_path: &Path) -> Result<BTreeMap<String, u64>> {
    let connection = Connection::open(database_path)?;
    let mut statement = connection
        .prepare("SELECT name, COALESCE(SUM(pgsize), 0) FROM dbstat GROUP BY name ORDER BY name")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut allocations = BTreeMap::new();
    for row in rows {
        let (name, bytes) = row?;
        let bytes = u64::try_from(bytes).map_err(|_| {
            KernelError::Reconciliation(format!(
                "SQLite dbstat returned a negative allocation for {name:?}"
            ))
        })?;
        allocations.insert(name, bytes);
    }
    Ok(allocations)
}

fn git_subject() -> Result<BenchmarkSubject> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let commit = command_text_in(root, "git", &["rev-parse", "HEAD"]).ok_or_else(|| {
        KernelError::Reconciliation("could not identify benchmark Git commit".to_owned())
    })?;
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(KernelError::Reconciliation(format!(
            "Git returned an invalid commit identity {commit:?}"
        )));
    }
    let status = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=normal"])
        .current_dir(root)
        .output()
        .map_err(|error| KernelError::io(root, error))?;
    if !status.status.success() {
        return Err(KernelError::Reconciliation(
            "could not determine benchmark worktree state".to_owned(),
        ));
    }
    Ok(BenchmarkSubject {
        git_commit: commit.to_ascii_lowercase(),
        git_clean: status.stdout.is_empty(),
    })
}

fn command_text(program: &str, arguments: &[&str]) -> Option<String> {
    command_text_in(Path::new(env!("CARGO_MANIFEST_DIR")), program, arguments)
}

fn command_text_in(directory: &Path, program: &str, arguments: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(directory)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn validate_evidence(schema_path: &Path, evidence: &BenchmarkEvidence) -> Result<()> {
    let schema = read_json(schema_path)?;
    jsonschema::draft202012::meta::validate(&schema).map_err(|error| {
        KernelError::Reconciliation(format!("benchmark evidence schema is invalid: {error}"))
    })?;
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .build(&schema)
        .map_err(|error| {
            KernelError::Reconciliation(format!(
                "benchmark evidence schema could not be compiled: {error}"
            ))
        })?;
    let document = serde_json::to_value(evidence)?;
    let mut issues = validator
        .iter_errors(&document)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    if issues.is_empty() {
        Ok(())
    } else {
        issues.sort_unstable();
        Err(KernelError::Reconciliation(format!(
            "benchmark evidence failed its schema: {}",
            issues.join("; ")
        )))
    }
}

fn write_new_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| KernelError::io(path, error))?;
    file.write_all(&bytes)
        .map_err(|error| KernelError::io(path, error))?;
    file.sync_all()
        .map_err(|error| KernelError::io(path, error))
}

fn write_new_text(path: &Path, value: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| KernelError::io(path, error))?;
    file.write_all(value.as_bytes())
        .map_err(|error| KernelError::io(path, error))?;
    file.sync_all()
        .map_err(|error| KernelError::io(path, error))
}

fn render_summary(case: &BenchmarkCase, evidence: &BenchmarkEvidence) -> String {
    let cpu = evidence
        .timing
        .cpu_seconds
        .map_or_else(|| "unavailable".to_owned(), |value| format!("{value:.3} s"));
    let rss = evidence.memory.peak_rss_bytes.map_or_else(
        || "unavailable".to_owned(),
        |value| format!("{value} bytes"),
    );
    format!(
        "# Benchmark {}\n\n\
         - Class: {}\n\
         - Description: {}\n\
         - Subject: `{}` ({})\n\
         - Fixture: `{}` — {} files, {} rows, {} columns/file, {} expanded bytes\n\
         - Guards: expanded <= {} bytes, free space >= {} bytes, RSS <= {} bytes, wall <= {} s, external RSS required {}\n\
         - Ingestion: {} ms, {:.2} rows/s, source/stored/pending {}/{}/{}\n\
         - Profiling: {} ms, result `{}`\n\
         - Peak RSS: {} ({})\n\
         - CPU: {} ({})\n\
         - SQLite final/peak: {}/{} bytes\n\
         - WAL final/peak: {}/{} bytes\n\
         - Integrity: {} ({} ms)\n\n\
         This Markdown is generated from `evidence.json`; it contains no independently entered measurements.\n",
        evidence.case_id,
        case.class,
        case.description,
        evidence.subject.git_commit,
        if evidence.subject.git_clean {
            "clean"
        } else {
            "dirty"
        },
        evidence.fixture.fixture_id,
        evidence.fixture.file_count,
        evidence.fixture.row_count,
        evidence.fixture.column_count,
        evidence.fixture.expanded_bytes,
        evidence.limits.maximum_expanded_bytes,
        evidence.limits.minimum_free_space_bytes,
        evidence.limits.maximum_peak_rss_bytes,
        evidence.limits.maximum_wall_seconds,
        evidence.limits.require_external_rss,
        evidence.intake.elapsed_ms,
        evidence.throughput.rows_per_second,
        evidence.intake.source_records,
        evidence.intake.stored_records,
        evidence.intake.pending_records,
        evidence.profiling.elapsed_ms,
        evidence.profiling.result_sha256,
        rss,
        evidence.memory.method,
        cpu,
        evidence.timing.cpu_method,
        evidence.storage.sqlite_final_bytes,
        display_optional(evidence.storage.sqlite_peak_bytes),
        evidence.storage.wal_final_bytes,
        display_optional(evidence.storage.wal_peak_bytes),
        evidence.outcome.integrity_ok,
        evidence.outcome.integrity_elapsed_ms,
    )
}

fn json_pointer<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |current, segment| current.get(*segment))
}

fn json_string(value: &Value, path: &[&str]) -> Result<String> {
    json_pointer(value, path)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| missing_json(&path.join(".")))
}

fn json_u64(value: &Value, path: &[&str]) -> Result<u64> {
    optional_json_u64(value, path).ok_or_else(|| missing_json(&path.join(".")))
}

fn optional_json_u64(value: &Value, path: &[&str]) -> Option<u64> {
    json_pointer(value, path).and_then(Value::as_u64)
}

fn json_bool(value: &Value, path: &[&str]) -> Result<bool> {
    optional_json_bool(value, path).ok_or_else(|| missing_json(&path.join(".")))
}

fn optional_json_bool(value: &Value, path: &[&str]) -> Option<bool> {
    json_pointer(value, path).and_then(Value::as_bool)
}

fn profile_result_hash(result: &Value) -> Result<String> {
    ["result_hash", "result_sha256", "profiling_result_hash"]
        .into_iter()
        .find_map(|field| result.get(field).and_then(Value::as_str))
        .map(str::to_owned)
        .ok_or_else(|| missing_json("result_hash"))
}

fn missing_json(field: &str) -> KernelError {
    KernelError::Reconciliation(format!(
        "benchmark child evidence is missing required field {field:?}"
    ))
}

fn max_option(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn max_options<const N: usize>(values: [Option<u64>; N]) -> Option<u64> {
    values.into_iter().flatten().max()
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[allow(clippy::cast_precision_loss)]
fn ratio(bytes: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        bytes as f64 / denominator as f64
    }
}

fn display_optional(value: Option<u64>) -> String {
    value.map_or_else(|| "unavailable".to_owned(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_time_parser_handles_supported_ps_shapes() {
        assert_eq!(parse_process_time("00:01"), Some(1.0));
        assert_eq!(parse_process_time("01:02:03.5"), Some(3_723.5));
        assert_eq!(parse_process_time("2-01:00:00"), Some(176_400.0));
        assert_eq!(parse_process_time("invalid"), None);
    }

    #[test]
    fn tokens_exclude_hostnames_and_free_text() {
        assert!(validate_token("apple-m4-16gb", "label").is_ok());
        assert!(validate_token("Operator's Mac", "label").is_err());
        assert!(validate_token("UPPER", "label").is_err());
    }

    #[test]
    fn checked_in_evidence_schema_is_valid() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/benchmark-evidence.schema.json");
        let schema = read_json(&path).expect("read benchmark schema");
        jsonschema::draft202012::meta::validate(&schema).expect("valid benchmark schema");
    }

    #[test]
    fn checked_in_matrix_has_every_required_class_and_resolves_inputs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let matrix_path = root.join("benchmarks/matrix.json");
        let matrix = load_matrix(&matrix_path).expect("load checked-in matrix");
        let classes = matrix
            .cases
            .iter()
            .map(|case| case.class.as_str())
            .collect::<HashSet<_>>();
        for required in [
            "smoke",
            "small-reference",
            "medium-representative",
            "large-local-evidence",
            "wide-row",
            "high-cardinality",
            "multi-file",
            "cancellation-recovery",
        ] {
            assert!(classes.contains(required), "missing class {required}");
        }
        let matrix_root = matrix_path.parent().expect("matrix parent");
        assert!(matrix_root.join(&matrix.evidence_schema).is_file());
        for case in matrix.cases {
            assert!(matrix_root.join(case.fixture_spec).is_file());
            assert!(matrix_root.join(case.intake_policy).is_file());
            assert!(matrix_root.join(case.profiling_policy).is_file());
        }
    }
}

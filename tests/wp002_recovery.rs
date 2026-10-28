use std::{
    fs::File,
    io::Write,
    path::Path,
    process::Command,
    sync::{Mutex, MutexGuard},
};

use migration_stream::{
    KernelError,
    intake::{IngestControl, ingest_package_with_control},
    policy::IntakePolicy,
    profiling::{
        DuplicateKeyPolicy, ProfilingControl, ProfilingPolicy, run_profile,
        run_profile_with_control,
    },
    state::{JobState, integrity_check, recover_workspace, workspace_status},
};
use rusqlite::Connection;
use tempfile::tempdir;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

#[path = "support/object_workspace.rs"]
mod object_workspace_support;

static RECOVERY_TEST_LOCK: Mutex<()> = Mutex::new(());

fn recovery_test_guard() -> MutexGuard<'static, ()> {
    RECOVERY_TEST_LOCK
        .lock()
        .expect("recovery test lock must not be poisoned")
}

fn write_csv_zip(path: &Path, rows: u64) {
    let file = File::create(path).expect("create ZIP");
    let mut writer = ZipWriter::new(file);
    writer
        .start_file(
            "data.csv",
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .expect("start CSV entry");
    writer.write_all(b"id,value\n").expect("write header");
    for row in 0..rows {
        writeln!(writer, "{row:06},value-{row:06}").expect("write CSV row");
    }
    writer.finish().expect("finish ZIP");
}

fn test_policy() -> IntakePolicy {
    IntakePolicy {
        minimum_free_space_bytes: 0,
        transaction_batch_rows: 2,
        ..IntakePolicy::default()
    }
}

fn profiling_policy() -> ProfilingPolicy {
    ProfilingPolicy {
        schema_version: 1,
        columns_per_pass: 1,
        row_batch_size: 2,
        maximum_profiled_files: 8,
        maximum_profiled_columns: 32,
        maximum_exact_distinct_values_per_column: 100,
        maximum_exact_distinct_bytes_per_column: 8 * 1024,
        maximum_exact_distinct_values_per_pass: 100,
        maximum_exact_distinct_bytes_per_pass: 8 * 1024,
        maximum_samples_per_column: 3,
        maximum_sample_value_bytes: 128,
        oversized_field_bytes: 64,
        duplicate_keys: vec![DuplicateKeyPolicy {
            name: "id".to_owned(),
            columns: vec!["id".to_owned()],
        }],
    }
}

#[test]
fn cooperative_cancellation_keeps_only_committed_batches_and_never_completes() {
    let _guard = recovery_test_guard();
    let temporary = tempdir().expect("temporary directory");
    let archive = temporary.path().join("source.zip");
    let workspace = temporary.path().join("workspace");
    write_csv_zip(&archive, 7);
    object_workspace_support::initialize_test_workspace(&workspace);

    let error = ingest_package_with_control(
        &archive,
        &workspace,
        &test_policy(),
        &IngestControl {
            cancel_file: None,
            cancel_after_batches: Some(1),
        },
    )
    .expect_err("ingestion must be cancelled");
    assert!(
        matches!(error, KernelError::Cancelled(_)),
        "unexpected cancellation error: {error:?}"
    );

    let status = workspace_status(&workspace).expect("workspace status");
    assert_eq!(status.complete_jobs, 0);
    assert_eq!(status.interrupted_jobs, 1);
    let job = &status.jobs[0];
    assert_eq!(job.state, JobState::Interrupted);
    assert_eq!(job.error_code.as_deref(), Some("COOPERATIVE_CANCELLED"));
    assert_eq!(job.source_records, 2);
    assert_eq!(job.stored_records, 2);
    assert_eq!(job.pending_records, 0);
    assert!(job.peak_database_bytes.is_some());
    assert!(job.peak_wal_bytes.is_some());
    assert!(job.peak_shm_bytes.is_some());

    let connection = Connection::open(workspace.join("state.sqlite3")).expect("open database");
    let durable_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM source_records", [], |row| row.get(0))
        .expect("durable rows");
    assert_eq!(durable_rows, 2);
    let completed_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM job_events WHERE state = 'COMPLETE'",
            [],
            |row| row.get(0),
        )
        .expect("completion events");
    assert_eq!(completed_events, 0);

    let integrity = integrity_check(&workspace).expect("integrity check");
    assert!(integrity.ok, "{:?}", integrity.issues);
}

#[test]
fn cancellation_before_spooling_is_durable_and_recovery_is_idempotent() {
    let _guard = recovery_test_guard();
    let temporary = tempdir().expect("temporary directory");
    let archive = temporary.path().join("source.zip");
    let workspace = temporary.path().join("workspace");
    let signal = temporary.path().join("cancel.signal");
    write_csv_zip(&archive, 3);
    object_workspace_support::initialize_test_workspace(&workspace);
    File::create(&signal).expect("create cancellation signal");

    let error = ingest_package_with_control(
        &archive,
        &workspace,
        &test_policy(),
        &IngestControl {
            cancel_file: Some(signal),
            cancel_after_batches: None,
        },
    )
    .expect_err("spooling must be cancelled");
    assert!(matches!(error, KernelError::Cancelled(_)));
    let status = workspace_status(&workspace).expect("workspace status");
    assert_eq!(status.complete_jobs, 0);
    assert_eq!(status.interrupted_jobs, 1);
    assert_eq!(status.jobs[0].stored_records, 0);

    let recovery = recover_workspace(&workspace).expect("recovery");
    assert_eq!(recovery.recovered_jobs, 0);
    assert_eq!(recovery.recovered_profiling_runs, 0);
    assert_eq!(recovery.removed_partial_files, 0);
    assert_eq!(recovery.quarantined_partial_files, 0);
    assert_eq!(recovery.deleted_tombstones, 0);
    assert_eq!(
        recovery.cleanup_pending,
        [] as [migration_stream::state::CleanupPending; 0]
    );
    let repeated = recover_workspace(&workspace).expect("repeated recovery");
    assert_eq!(repeated.recovered_jobs, 0);
    assert_eq!(repeated.recovered_profiling_runs, 0);
    assert_eq!(repeated.removed_partial_files, 0);
    assert_eq!(repeated.quarantined_partial_files, 0);
    assert_eq!(repeated.deleted_tombstones, 0);
    assert_eq!(
        repeated.cleanup_pending,
        [] as [migration_stream::state::CleanupPending; 0]
    );
    let packages = workspace.join("packages");
    if packages.exists() {
        assert!(
            std::fs::read_dir(packages)
                .expect("read packages")
                .all(|entry| {
                    entry
                        .expect("package entry")
                        .path()
                        .extension()
                        .is_none_or(|extension| extension != "part")
                })
        );
    }
    let connection = Connection::open(workspace.join("state.sqlite3")).expect("open database");
    let interrupted_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM job_events WHERE state = 'INTERRUPTED'",
            [],
            |row| row.get(0),
        )
        .expect("interrupted events");
    assert_eq!(interrupted_events, 1);
}

#[test]
fn profiling_cancellation_is_incomplete_and_a_rerun_is_reproducible() {
    let _guard = recovery_test_guard();
    let temporary = tempdir().expect("temporary directory");
    let archive = temporary.path().join("source.zip");
    let workspace = temporary.path().join("workspace");
    write_csv_zip(&archive, 7);
    object_workspace_support::initialize_test_workspace(&workspace);
    let ingest = ingest_package_with_control(
        &archive,
        &workspace,
        &test_policy(),
        &IngestControl::default(),
    )
    .expect("ingest fixture");

    let error = run_profile_with_control(
        &workspace,
        &ingest.job_id,
        &profiling_policy(),
        &ProfilingControl {
            cancel_file: None,
            cancel_after_batches: Some(1),
        },
    )
    .expect_err("profiling must be cancelled");
    assert!(
        matches!(error, KernelError::Cancelled(_)),
        "unexpected profiling error: {error:?}"
    );
    let connection = Connection::open(workspace.join("state.sqlite3")).expect("open database");
    let (state, code, complete_results): (String, Option<String>, i64) = connection
        .query_row(
            "SELECT state, error_code,
                    CASE WHEN result_json IS NULL AND result_hash IS NULL THEN 0 ELSE 1 END
             FROM profiling_runs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("cancelled profile");
    assert_eq!(state, "INTERRUPTED");
    assert_eq!(code.as_deref(), Some("COOPERATIVE_CANCELLED"));
    assert_eq!(complete_results, 0);
    drop(connection);

    let completed =
        run_profile(&workspace, &ingest.job_id, &profiling_policy()).expect("rerun profiling");
    assert!(!completed.reused_complete);
    let replay = run_profile(&workspace, &ingest.job_id, &profiling_policy())
        .expect("reuse complete profiling");
    assert!(replay.reused_complete);
    assert_eq!(replay.result_hash, completed.result_hash);
    assert_eq!(replay.result, completed.result);
    let integrity = integrity_check(&workspace).expect("integrity");
    assert!(integrity.ok, "{:?}", integrity.issues);
    assert_eq!(integrity.checked_profiling_runs, 2);
}

#[test]
fn hard_aborted_profiling_is_recovered_and_can_be_rerun() {
    let _guard = recovery_test_guard();
    let temporary = tempdir().expect("temporary directory");
    let archive = temporary.path().join("source.zip");
    let workspace = temporary.path().join("workspace");
    write_csv_zip(&archive, 7);
    object_workspace_support::initialize_test_workspace(&workspace);
    let ingest = ingest_package_with_control(
        &archive,
        &workspace,
        &test_policy(),
        &IngestControl::default(),
    )
    .expect("ingest fixture");
    let policy_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/profiling-policy.json");

    let output = Command::new(env!("CARGO_BIN_EXE_migration-stream"))
        .arg("profile")
        .arg("run")
        .arg("--workspace")
        .arg(&workspace)
        .arg("--job")
        .arg(&ingest.job_id)
        .arg("--policy")
        .arg(&policy_path)
        .env("MIGRATION_STREAM_TEST_CRASH_PROFILE_AFTER_BATCHES", "1")
        .output()
        .expect("run crash-injected profiler");
    assert!(!output.status.success());

    let recovery = recover_workspace(&workspace).expect("recover hard-aborted profile");
    assert_eq!(recovery.recovered_jobs, 0);
    assert_eq!(recovery.recovered_profiling_runs, 1);
    assert_eq!(recovery.removed_partial_files, 0);
    assert_eq!(recovery.quarantined_partial_files, 0);
    assert_eq!(recovery.deleted_tombstones, 0);
    assert_eq!(
        recovery.cleanup_pending,
        [] as [migration_stream::state::CleanupPending; 0]
    );
    let repeated = recover_workspace(&workspace).expect("repeat profile recovery");
    assert_eq!(repeated.recovered_jobs, 0);
    assert_eq!(repeated.recovered_profiling_runs, 0);
    assert_eq!(repeated.removed_partial_files, 0);
    assert_eq!(repeated.quarantined_partial_files, 0);
    assert_eq!(repeated.deleted_tombstones, 0);
    assert_eq!(
        repeated.cleanup_pending,
        [] as [migration_stream::state::CleanupPending; 0]
    );
    let connection = Connection::open(workspace.join("state.sqlite3")).expect("open database");
    let (state, result_count): (String, i64) = connection
        .query_row(
            "SELECT state,
                    CASE WHEN result_json IS NULL AND result_hash IS NULL THEN 0 ELSE 1 END
             FROM profiling_runs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("recovered profile");
    assert_eq!(state, "INTERRUPTED");
    assert_eq!(result_count, 0);
    let recovered_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM profiling_events
             WHERE state = 'INTERRUPTED' AND error_code = 'INTERRUPTED_RECOVERED'",
            [],
            |row| row.get(0),
        )
        .expect("recovered profiling events");
    assert_eq!(recovered_events, 1);
    drop(connection);

    let policy = ProfilingPolicy::from_path(&policy_path).expect("profiling policy");
    run_profile(&workspace, &ingest.job_id, &policy).expect("rerun recovered profile");
    let integrity = integrity_check(&workspace).expect("integrity");
    assert!(integrity.ok, "{:?}", integrity.issues);
    assert_eq!(integrity.checked_profiling_runs, 2);
}

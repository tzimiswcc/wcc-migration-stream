use std::{
    fs::{self, File},
    io::Write,
    path::Path,
};

use migration_stream::{
    KernelError,
    intake::ingest_package,
    policy::IntakePolicy,
    profiling::{
        DistinctMode, DuplicateKeyStatus, ProfilingControl, ProfilingPolicy, run_profile,
        run_profile_with_control,
    },
    state::integrity_check,
};
use rusqlite::Connection;
use serde_json::Value;
use tempfile::tempdir;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

#[path = "support/object_workspace.rs"]
mod object_workspace_support;

fn profiling_policy() -> ProfilingPolicy {
    ProfilingPolicy {
        schema_version: 1,
        columns_per_pass: 2,
        row_batch_size: 2,
        maximum_profiled_files: 4,
        maximum_profiled_columns: 32,
        maximum_exact_distinct_values_per_column: 16,
        maximum_exact_distinct_bytes_per_column: 1_024,
        maximum_exact_distinct_values_per_pass: 32,
        maximum_exact_distinct_bytes_per_pass: 2_048,
        maximum_samples_per_column: 3,
        maximum_sample_value_bytes: 4,
        oversized_field_bytes: 4,
        duplicate_keys: vec![migration_stream::profiling::DuplicateKeyPolicy {
            name: "id".to_owned(),
            columns: vec!["id".to_owned()],
        }],
    }
}

fn write_fixture(path: &Path) {
    let mut csv = csv::Writer::from_writer(Vec::new());
    csv.write_record(["id", "name", "note", "empty"])
        .expect("write header");
    csv.write_record(["001", "alpha", "line\nvalue", ""])
        .expect("write row");
    csv.write_record(["001", "beta", "éé", ""])
        .expect("write row");
    csv.write_record(["002", "", "quoted \"value\"", ""])
        .expect("write row");
    csv.write_record(["003", "delta", "東京", ""])
        .expect("write row");
    let bytes = csv.into_inner().expect("finish CSV");

    let file = File::create(path).expect("create ZIP");
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file("data/file.csv", options).expect("start CSV");
    zip.write_all(&bytes).expect("write CSV");
    zip.finish().expect("finish ZIP");
}

fn ingested_workspace(root: &Path) -> (std::path::PathBuf, String) {
    let source = root.join("fixture.zip");
    let workspace = root.join("workspace");
    write_fixture(&source);
    object_workspace_support::initialize_test_workspace(&workspace);
    let intake_policy = IntakePolicy {
        minimum_free_space_bytes: 0,
        transaction_batch_rows: 2,
        ..IntakePolicy::default()
    };
    let report = ingest_package(&source, &workspace, &intake_policy).expect("ingest fixture");
    (workspace, report.job_id)
}

fn expected_id_samples() -> Value {
    serde_json::json!([
        {
            "sha256": "7a3e6b16cb75f48fb897eff3ae732f3154f6d203b53f33660f01b4c3b6bc2df9",
            "utf8_bytes": 3,
            "decoded_length": 3,
            "value": "001"
        },
        {
            "sha256": "88c0413bfef1d0570a8a6f9c780a8d2c9e90c4d107551d62bf3cec9ff1f5b634",
            "utf8_bytes": 3,
            "decoded_length": 3,
            "value": "003"
        },
        {
            "sha256": "a1dd6837f284625bdb1cb68f1dbc85c5dc4d8b05bae24c94ed5f55c477326ea2",
            "utf8_bytes": 3,
            "decoded_length": 3,
            "value": "002"
        }
    ])
}

fn expected_name_samples() -> Value {
    serde_json::json!([
        {
            "sha256": "4f4a9410ffcdf895c4adb880659e9b5c0dd1f23a30790684340b3eaacb045398",
            "utf8_bytes": 5,
            "decoded_length": 5,
            "value": null
        },
        {
            "sha256": "8ed3f6ad685b959ead7022518e1af76cd816f8e8ec7ccdda1ed4018e8f2223f8",
            "utf8_bytes": 5,
            "decoded_length": 5,
            "value": null
        },
        {
            "sha256": "f44e64e75f3948e9f73f8dfa94721c4ce8cbb4f265c4790c702b2d41cfbf2753",
            "utf8_bytes": 4,
            "decoded_length": 4,
            "value": "beta"
        }
    ])
}

fn expected_note_samples() -> Value {
    serde_json::json!([
        {
            "sha256": "130016b2599bf7e5978cae78e528c67f29aa47eaa2490ffedd729df334a2790c",
            "utf8_bytes": 6,
            "decoded_length": 2,
            "value": null
        },
        {
            "sha256": "1dd5c4b8d4c961ee347d9c78778144d60ac02f4bf116e83f5893416ac811ecbf",
            "utf8_bytes": 14,
            "decoded_length": 14,
            "value": null
        },
        {
            "sha256": "7c0a962949a4ede62e7db95d2e824acc3dcb457863650811acbf50ccb4a4cff2",
            "utf8_bytes": 10,
            "decoded_length": 10,
            "value": null
        }
    ])
}

#[test]
fn textual_metrics_are_exact_bounded_and_persisted_atomically() {
    let directory = tempdir().expect("temporary directory");
    let (workspace, job_id) = ingested_workspace(directory.path());
    let report = run_profile(&workspace, &job_id, &profiling_policy()).expect("profile fixture");

    assert!(!report.reused_complete);
    assert_eq!(report.result.file_count, 1);
    assert_eq!(report.result.row_count, 4);
    assert_eq!(report.result.column_count, 4);
    let file = &report.result.files[0];
    assert_eq!(file.path, "data/file.csv");
    assert_eq!(file.row_count, 4);
    assert_eq!(file.column_count, 4);
    assert_eq!(file.duplicate_keys.len(), 1);
    assert_eq!(file.duplicate_keys[0].status, DuplicateKeyStatus::Exact);
    assert_eq!(file.duplicate_keys[0].distinct_key_count, Some(3));
    assert_eq!(file.duplicate_keys[0].duplicate_row_count, Some(1));

    let id = &file.columns[0];
    assert_eq!(id.empty_count, 0);
    assert_eq!(id.nonempty_count, 4);
    assert_eq!(id.min_decoded_length, Some(3));
    assert_eq!(id.max_decoded_length, Some(3));
    assert_eq!(id.distinct.mode, DistinctMode::Exact);
    assert_eq!(id.distinct.count, Some(3));
    assert_eq!(
        serde_json::to_value(&id.samples).expect("id samples JSON"),
        expected_id_samples()
    );

    let name = &file.columns[1];
    assert_eq!(name.empty_count, 1);
    assert_eq!(name.nonempty_count, 3);
    assert_eq!(name.min_decoded_length, Some(4));
    assert_eq!(name.max_decoded_length, Some(5));
    assert_eq!(name.oversized_count, 2);
    assert_eq!(
        serde_json::to_value(&name.samples).expect("name samples JSON"),
        expected_name_samples()
    );

    let note = &file.columns[2];
    assert_eq!(note.empty_count, 0);
    assert_eq!(note.nonempty_count, 4);
    assert_eq!(note.min_decoded_length, Some(2));
    assert_eq!(
        serde_json::to_value(&note.samples).expect("note samples JSON"),
        expected_note_samples()
    );

    let empty = &file.columns[3];
    assert_eq!(empty.empty_count, 4);
    assert_eq!(empty.nonempty_count, 0);
    assert_eq!(empty.min_decoded_length, None);
    assert_eq!(empty.max_decoded_length, None);
    assert_eq!(empty.distinct.count, Some(1));
    assert!(empty.samples.is_empty());

    let database = Connection::open(workspace.join("state.sqlite3")).expect("open state");
    let counts: (i64, i64, i64) = database
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM profiling_runs WHERE state = 'COMPLETE'),
                 (SELECT COUNT(*) FROM profiling_files),
                 (SELECT COUNT(*) FROM profiling_columns)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query profiling evidence");
    assert_eq!(counts, (1, 1, 4));
    drop(database);
    let integrity = integrity_check(&workspace).expect("verify workspace");
    assert!(integrity.ok, "{:?}", integrity.issues);
    assert_eq!(integrity.checked_profiling_runs, 1);
}

#[test]
fn zero_sample_bound_retains_no_sample_hashes_or_values() {
    let directory = tempdir().expect("temporary directory");
    let (workspace, job_id) = ingested_workspace(directory.path());
    let mut policy = profiling_policy();
    policy.maximum_samples_per_column = 0;
    policy.maximum_sample_value_bytes = 1_048_576;
    policy.validate().expect("zero sample retention is valid");

    let report = run_profile(&workspace, &job_id, &policy).expect("sample-free profile");
    assert!(
        report
            .result
            .files
            .iter()
            .flat_map(|file| &file.columns)
            .all(|column| column.samples.is_empty())
    );

    let database = Connection::open(workspace.join("state.sqlite3")).expect("open state");
    let samples = database
        .prepare("SELECT samples_json FROM profiling_columns WHERE run_id = ?1")
        .expect("prepare sample evidence")
        .query_map([&report.run_id], |row| row.get::<_, String>(0))
        .expect("query sample evidence")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect sample evidence");
    assert!(!samples.is_empty());
    assert!(samples.iter().all(|sample_json| sample_json == "[]"));
    let result_json: String = database
        .query_row(
            "SELECT result_json FROM profiling_runs WHERE id = ?1",
            [&report.run_id],
            |row| row.get(0),
        )
        .expect("read complete result");
    assert!(!result_json.contains("\"sha256\""));
    assert!(!result_json.contains("\"value\""));
}

#[test]
fn bound_state_is_explicit_and_identical_complete_identity_is_reused() {
    let directory = tempdir().expect("temporary directory");
    let (workspace, job_id) = ingested_workspace(directory.path());
    let mut policy = profiling_policy();
    policy.maximum_exact_distinct_values_per_column = 2;
    policy.maximum_exact_distinct_values_per_pass = 4;

    let first = run_profile(&workspace, &job_id, &policy).expect("first profile");
    let id = &first.result.files[0].columns[0];
    assert_eq!(id.distinct.mode, DistinctMode::NotComputedDueToBound);
    assert_eq!(id.distinct.count, None);

    let second = run_profile(&workspace, &job_id, &policy).expect("reuse profile");
    assert!(second.reused_complete);
    assert_eq!(second.run_id, first.run_id);
    assert_eq!(second.identity_hash, first.identity_hash);
    assert_eq!(second.result_hash, first.result_hash);
    assert_eq!(second.result, first.result);

    let schema_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("schemas/profiling-result.schema.json");
    let schema: Value =
        serde_json::from_slice(&fs::read(schema_path).expect("read schema")).expect("parse schema");
    jsonschema::draft202012::meta::validate(&schema).expect("valid JSON Schema");
    let validator = jsonschema::draft202012::options()
        .build(&schema)
        .expect("compile JSON Schema");
    assert!(validator.is_valid(&serde_json::to_value(&first.result).expect("result JSON")));
}

#[test]
fn canonical_result_hash_excludes_variable_job_provenance() {
    let directory = tempdir().expect("temporary directory");
    let (workspace, first_job_id) = ingested_workspace(directory.path());
    let policy = profiling_policy();
    let first = run_profile(&workspace, &first_job_id, &policy).expect("first profile");

    let source = directory.path().join("fixture.zip");
    let intake_policy = IntakePolicy {
        minimum_free_space_bytes: 0,
        transaction_batch_rows: 2,
        ..IntakePolicy::default()
    };
    let second_ingest =
        ingest_package(&source, &workspace, &intake_policy).expect("repeat ingestion");
    assert_ne!(second_ingest.job_id, first_job_id);
    let second = run_profile(&workspace, &second_ingest.job_id, &policy).expect("second profile");

    assert_ne!(second.result.job_id, first.result.job_id);
    assert_eq!(second.identity_hash, first.identity_hash);
    assert_eq!(second.result_hash, first.result_hash);
    let mut first_semantics = serde_json::to_value(&first.result).expect("first result JSON");
    let mut second_semantics = serde_json::to_value(&second.result).expect("second result JSON");
    first_semantics
        .as_object_mut()
        .expect("first result object")
        .remove("job_id");
    second_semantics
        .as_object_mut()
        .expect("second result object")
        .remove("job_id");
    assert_eq!(second_semantics, first_semantics);

    let integrity = integrity_check(&workspace).expect("verify repeated workspace");
    assert!(integrity.ok, "{:?}", integrity.issues);
    assert_eq!(integrity.checked_profiling_runs, 2);
}

#[test]
fn cooperative_cancellation_preserves_interrupted_attempt_without_result() {
    let directory = tempdir().expect("temporary directory");
    let (workspace, job_id) = ingested_workspace(directory.path());
    let mut policy = profiling_policy();
    policy.row_batch_size = 1;
    let error = run_profile_with_control(
        &workspace,
        &job_id,
        &policy,
        &ProfilingControl {
            cancel_file: None,
            cancel_after_batches: Some(1),
        },
    )
    .expect_err("cancel profile");
    assert!(matches!(error, KernelError::Cancelled(_)));

    let database = Connection::open(workspace.join("state.sqlite3")).expect("open state");
    let interrupted: (String, String, Option<String>, Option<String>) = database
        .query_row(
            "SELECT state, error_code, result_json, result_hash
             FROM profiling_runs ORDER BY created_at, id LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query interrupted profile");
    assert_eq!(interrupted.0, "INTERRUPTED");
    assert_eq!(interrupted.1, "COOPERATIVE_CANCELLED");
    assert_eq!(interrupted.2, None);
    assert_eq!(interrupted.3, None);
    let children: i64 = database
        .query_row(
            "SELECT (SELECT COUNT(*) FROM profiling_files)
                    + (SELECT COUNT(*) FROM profiling_columns)",
            [],
            |row| row.get(0),
        )
        .expect("query incomplete child evidence");
    assert_eq!(children, 0);
    drop(database);

    let completed = run_profile(&workspace, &job_id, &policy).expect("rerun profile");
    assert!(!completed.reused_complete);
    let database = Connection::open(workspace.join("state.sqlite3")).expect("open state");
    let states: (i64, i64) = database
        .query_row(
            "SELECT
                 SUM(CASE WHEN state = 'INTERRUPTED' THEN 1 ELSE 0 END),
                 SUM(CASE WHEN state = 'COMPLETE' THEN 1 ELSE 0 END)
             FROM profiling_runs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query profile attempts");
    assert_eq!(states, (1, 1));
}

#[test]
fn version_controlled_policy_loads_and_validates() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config/profiling-policy.json");
    let policy = ProfilingPolicy::from_path(&path).expect("load profiling policy");
    assert_eq!(policy.schema_version, 1);
    assert_eq!(policy.duplicate_keys[0].columns, ["id"]);
}

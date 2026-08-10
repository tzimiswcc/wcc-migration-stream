use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};

use migration_stream::{
    KernelError,
    fixture::{
        Cardinality, Compressibility, FieldLengthSpec, FixtureSpec, MalformedKind, MalformedSpec,
        SkewSpec, generate_fixture,
    },
    intake::ingest_package,
    policy::IntakePolicy,
    state::{JobState, workspace_status},
};
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::tempdir;
use zip::ZipArchive;

#[path = "support/object_workspace.rs"]
mod object_workspace_support;

fn spec(name: &str) -> FixtureSpec {
    FixtureSpec {
        schema_version: 1,
        fixture_name: name.to_owned(),
        seed: 0x0123_4567_89ab_cdef,
        file_count: 2,
        rows_per_file: 64,
        columns: 6,
        field_lengths: FieldLengthSpec {
            minimum: 8,
            typical: 24,
            maximum: 64,
        },
        compressibility: Compressibility::Medium,
        empty_frequency: 0.1,
        cardinality: Cardinality::Medium,
        duplicate_key_frequency: 0.2,
        multiline_frequency: 0.05,
        delimiter: ",".to_owned(),
        delimiter_in_field_frequency: 0.05,
        escaped_quote_frequency: 0.05,
        unicode_frequency: 0.05,
        leading_zero_id_frequency: 0.5,
        skew: SkewSpec {
            hot_value_frequency: 0.25,
        },
        malformed: None,
        maximum_expanded_bytes: 8 * 1024 * 1024,
        maximum_output_bytes: 8 * 1024 * 1024,
    }
}

fn sha256(path: &Path) -> String {
    let mut file = File::open(path).expect("open file for hash");
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = file.read(&mut buffer).expect("read file for hash");
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    format!("{:x}", digest.finalize())
}

fn zip_entry(path: &Path, name: &str) -> Vec<u8> {
    let file = File::open(path).expect("open ZIP");
    let mut archive = ZipArchive::new(file).expect("read ZIP");
    let mut entry = archive.by_name(name).expect("find ZIP entry");
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).expect("read ZIP entry");
    bytes
}

#[test]
fn identical_specs_produce_byte_identical_archives_and_manifests() {
    let first_directory = tempdir().expect("first temporary directory");
    let second_directory = tempdir().expect("second temporary directory");
    let fixture_spec = spec("deterministic");

    let first = generate_fixture(&fixture_spec, first_directory.path()).expect("first fixture");
    let second = generate_fixture(&fixture_spec, second_directory.path()).expect("second fixture");

    assert_eq!(sha256(&first.archive_path), sha256(&second.archive_path));
    assert_eq!(first.manifest, second.manifest);
    assert_eq!(
        fs::read(&first.manifest_path).expect("first manifest"),
        fs::read(&second.manifest_path).expect("second manifest")
    );
    assert_eq!(
        first.manifest.fixture_identity,
        second.manifest.fixture_identity
    );
    assert_eq!(first.manifest.valid_archive.files.len(), 2);
    assert_eq!(
        first.manifest.valid_archive.files[0].path,
        "data/file-0001.csv"
    );
    assert_eq!(
        first.manifest.valid_archive.files[1].path,
        "data/file-0002.csv"
    );

    let document: Value =
        serde_json::from_slice(&fs::read(&first.manifest_path).expect("read generated manifest"))
            .expect("parse generated manifest");
    let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schemas/synthetic-fixture-manifest.schema.json");
    let schema: Value =
        serde_json::from_slice(&fs::read(schema_path).expect("read schema")).expect("parse schema");
    jsonschema::draft202012::meta::validate(&schema).expect("valid JSON Schema");
    let validator = jsonschema::draft202012::options()
        .build(&schema)
        .expect("compile JSON Schema");
    assert!(validator.is_valid(&document));
}

#[test]
fn smoke_fixture_has_cross_platform_golden_archive_identity() {
    let spec_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks/specs/smoke.json");
    let fixture_spec: FixtureSpec =
        serde_json::from_slice(&fs::read(spec_path).expect("read smoke specification"))
            .expect("parse smoke specification");
    let directory = tempdir().expect("temporary directory");

    let fixture = generate_fixture(&fixture_spec, directory.path()).expect("generate smoke");

    assert_eq!(
        fixture.manifest.valid_archive.sha256,
        "1a9ce50da7048fd9d70ee9b6490ad2d9aee7fb696f3d50597a5a33bc2d3ab05c"
    );
    assert_eq!(
        fixture.manifest.fixture_identity,
        "47a6534284f9ff181f6ecbc8e0515313308a1969e1778e686cdd9be331f0f9fc"
    );
}

#[test]
fn seed_and_shape_changes_change_fixture_identity_and_package_hash() {
    let directory = tempdir().expect("temporary directory");
    let baseline_spec = spec("identity-sensitivity");
    let mut changed_seed_spec = baseline_spec.clone();
    changed_seed_spec.seed = baseline_spec.seed.wrapping_add(1);
    let mut changed_shape_spec = baseline_spec.clone();
    changed_shape_spec.rows_per_file += 1;

    let baseline = generate_fixture(&baseline_spec, &directory.path().join("baseline"))
        .expect("baseline fixture");
    let changed_seed = generate_fixture(&changed_seed_spec, &directory.path().join("changed-seed"))
        .expect("changed-seed fixture");
    let changed_shape =
        generate_fixture(&changed_shape_spec, &directory.path().join("changed-shape"))
            .expect("changed-shape fixture");

    assert_eq!(
        baseline.manifest.valid_archive.sha256,
        sha256(&baseline.archive_path)
    );
    assert_eq!(
        changed_seed.manifest.valid_archive.sha256,
        sha256(&changed_seed.archive_path)
    );
    assert_eq!(
        changed_shape.manifest.valid_archive.sha256,
        sha256(&changed_shape.archive_path)
    );
    assert_ne!(
        baseline.manifest.fixture_identity,
        changed_seed.manifest.fixture_identity
    );
    assert_ne!(
        baseline.manifest.fixture_identity,
        changed_shape.manifest.fixture_identity
    );
    assert_ne!(
        changed_seed.manifest.fixture_identity,
        changed_shape.manifest.fixture_identity
    );
    assert_ne!(
        baseline.manifest.valid_archive.sha256,
        changed_seed.manifest.valid_archive.sha256
    );
    assert_ne!(
        baseline.manifest.valid_archive.sha256,
        changed_shape.manifest.valid_archive.sha256
    );
    assert_ne!(
        changed_seed.manifest.valid_archive.sha256,
        changed_shape.manifest.valid_archive.sha256
    );
}

#[test]
fn generated_csv_exercises_text_edges_and_configured_key_duplicates() {
    let directory = tempdir().expect("temporary directory");
    let mut fixture_spec = spec("text-edges");
    fixture_spec.file_count = 1;
    fixture_spec.rows_per_file = 3;
    fixture_spec.columns = 3;
    fixture_spec.field_lengths = FieldLengthSpec {
        minimum: 32,
        typical: 48,
        maximum: 64,
    };
    fixture_spec.empty_frequency = 0.0;
    fixture_spec.duplicate_key_frequency = 1.0;
    fixture_spec.multiline_frequency = 1.0;
    fixture_spec.delimiter = ";".to_owned();
    fixture_spec.delimiter_in_field_frequency = 1.0;
    fixture_spec.escaped_quote_frequency = 1.0;
    fixture_spec.unicode_frequency = 1.0;
    fixture_spec.leading_zero_id_frequency = 1.0;
    fixture_spec.cardinality = Cardinality::High;
    fixture_spec.compressibility = Compressibility::Low;

    let generated = generate_fixture(&fixture_spec, directory.path()).expect("generate fixture");
    let bytes = zip_entry(&generated.archive_path, "data/file-0001.csv");
    let raw = String::from_utf8(bytes.clone()).expect("valid UTF-8 fixture");
    assert!(raw.contains("Δ東"));
    assert!(raw.contains("\"\""), "CSV must escape embedded quotes");
    assert!(raw.contains(';'));
    assert!(raw.contains("\n;"), "quoted multiline value must retain LF");
    assert!(!raw.contains('\r'), "generator must use LF terminators");

    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .from_reader(bytes.as_slice());
    assert_eq!(
        reader
            .headers()
            .expect("headers")
            .iter()
            .collect::<Vec<_>>(),
        vec!["id", "column_0002", "column_0003"]
    );
    let records = reader
        .records()
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("parse generated CSV");
    assert_eq!(records.len(), 3);
    assert!(records.iter().all(|record| record.get(0) == Some("K01")));
    assert!(records.iter().all(|record| {
        record
            .get(1)
            .is_some_and(|value| value.contains('\n') && value.contains(';') && value.contains('"'))
    }));
}

#[test]
fn malformed_data_is_opt_in_and_isolated_in_a_separate_archive() {
    let directory = tempdir().expect("temporary directory");
    let mut fixture_spec = spec("negative-case");
    fixture_spec.file_count = 1;
    fixture_spec.rows_per_file = 2;
    fixture_spec.malformed = Some(MalformedSpec {
        kind: MalformedKind::InvalidUtf8,
    });

    let generated = generate_fixture(&fixture_spec, directory.path()).expect("generate fixture");
    let malformed_path = generated
        .malformed_archive_path
        .as_ref()
        .expect("separate malformed archive");
    assert_ne!(generated.archive_path, *malformed_path);
    assert!(generated.manifest.malformed_archive.is_some());
    assert!(
        generated
            .manifest
            .valid_archive
            .files
            .iter()
            .all(|file| !file.intentionally_malformed)
    );
    assert!(
        generated
            .manifest
            .malformed_archive
            .as_ref()
            .expect("malformed evidence")
            .files
            .iter()
            .all(|file| file.intentionally_malformed)
    );

    let valid = zip_entry(&generated.archive_path, "data/file-0001.csv");
    assert!(String::from_utf8(valid).is_ok());
    let invalid = zip_entry(malformed_path, "malformed/malformed.csv");
    assert!(String::from_utf8(invalid).is_err());
}

#[test]
fn existing_outputs_are_never_overwritten() {
    let directory = tempdir().expect("temporary directory");
    let fixture_spec = spec("preserve-existing");
    let generated = generate_fixture(&fixture_spec, directory.path()).expect("generate fixture");
    let archive_hash = sha256(&generated.archive_path);
    let manifest = fs::read(&generated.manifest_path).expect("read manifest");

    let error = generate_fixture(&fixture_spec, directory.path()).expect_err("reject overwrite");
    assert_eq!(error.code(), "IO");
    assert_eq!(sha256(&generated.archive_path), archive_hash);
    assert_eq!(
        fs::read(&generated.manifest_path).expect("read preserved manifest"),
        manifest
    );
}

#[test]
fn expanded_and_output_ceilings_remove_partial_outputs() {
    for (name, expanded, output) in [
        ("expanded-cap", 8, 1_000_000),
        ("output-cap", 1_000_000, 32),
    ] {
        let directory = tempdir().expect("temporary directory");
        let mut fixture_spec = spec(name);
        fixture_spec.file_count = 1;
        fixture_spec.rows_per_file = 1;
        fixture_spec.maximum_expanded_bytes = expanded;
        fixture_spec.maximum_output_bytes = output;

        let error = generate_fixture(&fixture_spec, directory.path()).expect_err("enforce ceiling");
        assert!(matches!(error.code(), "IO" | "POLICY_INVALID"));
        assert!(!directory.path().join(format!("{name}.zip")).exists());
        assert!(
            !directory
                .path()
                .join(format!("{name}.manifest.json"))
                .exists()
        );
    }
}

#[test]
fn generated_256_column_companion_is_rejected_without_partial_acceptance() {
    let directory = tempdir().expect("temporary directory");
    let mut fixture_spec = spec("wide-row-over-limit");
    fixture_spec.file_count = 1;
    fixture_spec.rows_per_file = 2;
    fixture_spec.columns = 256;
    fixture_spec.field_lengths = FieldLengthSpec {
        minimum: 2,
        typical: 4,
        maximum: 8,
    };
    let generated =
        generate_fixture(&fixture_spec, &directory.path().join("fixture")).expect("wide fixture");
    assert_eq!(generated.manifest.valid_archive.files[0].columns, 256);

    let workspace = directory.path().join("workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let policy = IntakePolicy {
        minimum_free_space_bytes: 0,
        maximum_csv_columns: 255,
        ..IntakePolicy::default()
    };
    let error = ingest_package(&generated.archive_path, &workspace, &policy)
        .expect_err("256 columns must exceed the 255-column companion policy");
    assert!(matches!(
        error,
        KernelError::CsvColumnLimit {
            actual: 256,
            limit: 255,
            ..
        }
    ));
    assert_eq!(error.code(), "CSV_COLUMN_LIMIT");

    let status = workspace_status(&workspace).expect("failed workspace status");
    assert_eq!(status.active_jobs, 0);
    assert_eq!(status.complete_jobs, 0);
    assert_eq!(status.failed_jobs, 1);
    assert_eq!(status.jobs.len(), 1);
    assert_eq!(status.jobs[0].state, JobState::Failed);
    assert_eq!(
        status.jobs[0].error_code.as_deref(),
        Some("CSV_COLUMN_LIMIT")
    );
    assert_eq!(status.jobs[0].source_records, 0);
    assert_eq!(status.jobs[0].stored_records, 0);
    assert_eq!(status.jobs[0].pending_records, 0);

    let database = Connection::open(workspace.join("state.sqlite3")).expect("open failed state");
    let accepted_records: i64 = database
        .query_row("SELECT COUNT(*) FROM source_records", [], |row| row.get(0))
        .expect("count accepted records");
    let complete_jobs: i64 = database
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE state = 'COMPLETE'",
            [],
            |row| row.get(0),
        )
        .expect("count complete jobs");
    assert_eq!(accepted_records, 0);
    assert_eq!(complete_jobs, 0);
}

#[test]
fn spec_loader_rejects_unknown_fields_and_unsafe_output_names() {
    let directory = tempdir().expect("temporary directory");
    let path = directory.path().join("spec.json");
    let mut document = serde_json::to_value(spec("safe-name")).expect("serialize fixture spec");
    document["unexpected"] = Value::Bool(true);
    fs::write(
        &path,
        serde_json::to_vec_pretty(&document).expect("serialize JSON"),
    )
    .expect("write fixture spec");
    assert_eq!(
        FixtureSpec::from_path(&path)
            .expect_err("unknown field must fail")
            .code(),
        "JSON"
    );

    let mut unsafe_spec = spec("safe-name");
    unsafe_spec.fixture_name = "../escape".to_owned();
    assert_eq!(
        unsafe_spec.validate().expect_err("unsafe name").code(),
        "POLICY_INVALID"
    );
}

#[test]
fn checked_in_matrix_specs_are_valid_and_match_the_approved_shapes() {
    let specifications = [
        ("smoke.json", 1, 1_000, 8),
        ("small-reference.json", 1, 25_000, 16),
        ("medium-representative.json", 1, 250_000, 16),
        ("large-local.json", 1, 800_000, 16),
        ("wide-row.json", 2, 20_000, 256),
        ("high-cardinality.json", 1, 250_000, 16),
        ("multi-file.json", 32, 10_000, 10),
        ("cancellation-recovery.json", 1, 600_000, 16),
    ];
    for (file_name, file_count, rows_per_file, columns) in specifications {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("benchmarks/specs")
            .join(file_name);
        let fixture_spec = FixtureSpec::from_path(&path).expect("valid checked-in fixture spec");
        assert_eq!(fixture_spec.file_count, file_count);
        assert_eq!(fixture_spec.rows_per_file, rows_per_file);
        assert_eq!(fixture_spec.columns, columns);
        assert_eq!(fixture_spec.delimiter, ",");
    }

    let mut small = FixtureSpec::from_path(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks/specs/small-reference.json"),
    )
    .expect("small spec");
    let mut medium = FixtureSpec::from_path(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks/specs/medium-representative.json"),
    )
    .expect("medium spec");
    let mut large = FixtureSpec::from_path(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks/specs/large-local.json"),
    )
    .expect("large spec");
    for fixture_spec in [&mut small, &mut medium, &mut large] {
        fixture_spec.fixture_name = "shared-profile".to_owned();
        fixture_spec.rows_per_file = 1;
    }
    assert_eq!(small, medium);
    assert_eq!(medium, large);
}

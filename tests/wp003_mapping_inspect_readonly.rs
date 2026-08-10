use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use migration_stream::{
    KernelError,
    canonical::{canonical_hash, canonical_json_string},
    intake::ingest_package,
    mapping::{inspect_mapping, validate_mapping},
    policy::IntakePolicy,
    profile_registry::{RegistryLimits, approve_profile, inspect_profile, register_profile},
    state::SCHEMA_VERSION,
    validation::validation_status,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::TempDir;

#[path = "support/object_workspace.rs"]
mod object_workspace_support;

const MAPPING_LIMIT: u64 = 2 * 1024 * 1024;
const MAPPING_SCHEMA_LIMIT: u64 = 4 * 1024 * 1024;
const PROFILE_ID: &str = "synthetic.customer.readiness";
const PROFILE_VERSION: &str = "1.0.0";
const MAPPING_ID: &str = "synthetic.customer.valid";
const MAPPING_VERSION: &str = "1.0.0";
const PROFILE_APPROVALS_UPDATE_GUARD_SQL: &str = "CREATE TRIGGER profile_approvals_no_update BEFORE UPDATE ON profile_approvals BEGIN SELECT RAISE(ABORT, 'profile approval history is immutable'); END";
const MAPPING_ATTEMPTS_UPDATE_GUARD_SQL: &str = "CREATE TRIGGER mapping_validation_attempts_no_update BEFORE UPDATE ON mapping_validation_attempts BEGIN SELECT RAISE(ABORT, 'mapping validation attempt history is immutable'); END";
const MAPPING_VERSIONS_UPDATE_GUARD_SQL: &str = "CREATE TRIGGER mapping_versions_no_update BEFORE UPDATE ON mapping_versions BEGIN SELECT RAISE(ABORT, 'mapping version history is immutable'); END";

struct RegisteredMappingFixture {
    temporary: TempDir,
    workspace: PathBuf,
    job_id: String,
    source_hash: String,
    mapping_hash: String,
}

fn project_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn with_immutability_guard_disabled<T>(
    connection: &Connection,
    name: &str,
    exact_sql: &str,
    action: impl FnOnce(&Connection) -> T,
) -> T {
    connection
        .execute_batch(&format!("DROP TRIGGER {name};"))
        .expect("open test-only immutable-history mutation window");
    let result = action(connection);
    connection
        .execute_batch(exact_sql)
        .expect("restore exact immutable-history guard");
    result
}

fn create_sparse_file(path: &Path, size: u64) {
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .expect("create bounded-read fixture");
    file.set_len(size).expect("size bounded-read fixture");
}

fn prepare_registered_mapping() -> RegisteredMappingFixture {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let intake = ingest_package(
        &project_path("fixtures/packages/valid.zip"),
        &workspace,
        &IntakePolicy {
            minimum_free_space_bytes: 0,
            transaction_batch_rows: 2,
            ..IntakePolicy::default()
        },
    )
    .expect("ingest governed synthetic package");
    let draft = register_profile(
        &workspace,
        &project_path("schemas/migration-object-profile.schema.json"),
        &project_path("fixtures/wp003/synthetic-profile.reviewed.json"),
        &RegistryLimits::default(),
    )
    .expect("register governed synthetic profile");
    let approved = approve_profile(
        &workspace,
        PROFILE_ID,
        PROFILE_VERSION,
        &draft.profile_hash,
        "WP-003 mapping inspection regression",
        "Authorize this exact synthetic profile for local mapping and validation only",
        true,
        &RegistryLimits::default(),
    )
    .expect("approve exact synthetic profile");
    let mapping_path = temporary.path().join("mapping.json");
    let template = fs::read_to_string(project_path("fixtures/wp003/valid-mapping.template.json"))
        .expect("read mapping template");
    fs::write(
        &mapping_path,
        template.replace("__PROFILE_SHA256__", &approved.profile_hash),
    )
    .expect("write exact governed mapping");
    let mapping = validate_mapping(
        &workspace,
        &intake.job_id,
        &project_path("schemas/source-mapping-version.schema.json"),
        &project_path("schemas/migration-object-profile.schema.json"),
        &mapping_path,
    )
    .expect("register exact governed mapping");
    RegisteredMappingFixture {
        temporary,
        workspace,
        job_id: intake.job_id,
        source_hash: intake.source_hash,
        mapping_hash: mapping.mapping_hash,
    }
}

fn read_document(fixture: &RegisteredMappingFixture) -> Value {
    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open authoritative state");
    let value: String = connection
        .query_row(
            "SELECT document_json FROM mapping_versions
             WHERE mapping_id = ?1 AND mapping_version = ?2",
            params![MAPPING_ID, MAPPING_VERSION],
            |row| row.get(0),
        )
        .expect("read stored mapping document");
    serde_json::from_str(&value).expect("parse stored mapping document")
}

fn read_coverage(fixture: &RegisteredMappingFixture) -> Value {
    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open authoritative state");
    let value: String = connection
        .query_row(
            "SELECT coverage_json FROM mapping_versions
             WHERE mapping_id = ?1 AND mapping_version = ?2",
            params![MAPPING_ID, MAPPING_VERSION],
            |row| row.get(0),
        )
        .expect("read stored mapping coverage");
    serde_json::from_str(&value).expect("parse stored mapping coverage")
}

fn replace_document(fixture: &RegisteredMappingFixture, document: &Value, rehash: bool) -> String {
    let document_json = canonical_json_string(document).expect("canonical mapping document");
    let document_hash = canonical_hash(document).expect("hash mapping document");
    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open authoritative state for deliberate tamper");
    with_immutability_guard_disabled(
        &connection,
        "mapping_versions_no_update",
        MAPPING_VERSIONS_UPDATE_GUARD_SQL,
        |connection| {
            if rehash {
                connection
                    .execute(
                        "UPDATE mapping_versions SET document_json = ?1, mapping_hash = ?2
                         WHERE mapping_id = ?3 AND mapping_version = ?4",
                        params![document_json, document_hash, MAPPING_ID, MAPPING_VERSION],
                    )
                    .expect("replace and rehash mapping document");
            } else {
                connection
                    .execute(
                        "UPDATE mapping_versions SET document_json = ?1
                         WHERE mapping_id = ?2 AND mapping_version = ?3",
                        params![document_json, MAPPING_ID, MAPPING_VERSION],
                    )
                    .expect("replace mapping document without hash");
            }
        },
    );
    document_hash
}

fn replace_coverage(fixture: &RegisteredMappingFixture, coverage: &Value, rehash: bool) {
    let coverage_json = canonical_json_string(coverage).expect("canonical mapping coverage");
    let coverage_hash = canonical_hash(coverage).expect("hash mapping coverage");
    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open authoritative state for deliberate tamper");
    with_immutability_guard_disabled(
        &connection,
        "mapping_versions_no_update",
        MAPPING_VERSIONS_UPDATE_GUARD_SQL,
        |connection| {
            if rehash {
                connection
                    .execute(
                        "UPDATE mapping_versions SET coverage_json = ?1, coverage_hash = ?2
                         WHERE mapping_id = ?3 AND mapping_version = ?4",
                        params![coverage_json, coverage_hash, MAPPING_ID, MAPPING_VERSION],
                    )
                    .expect("replace and rehash mapping coverage");
            } else {
                connection
                    .execute(
                        "UPDATE mapping_versions SET coverage_json = ?1
                         WHERE mapping_id = ?2 AND mapping_version = ?3",
                        params![coverage_json, MAPPING_ID, MAPPING_VERSION],
                    )
                    .expect("replace mapping coverage without hash");
            }
        },
    );
}

fn expect_inspection_integrity(fixture: &RegisteredMappingFixture, message: &str) {
    let error = expect_error_code(
        inspect_mapping(&fixture.workspace, MAPPING_ID, MAPPING_VERSION),
        "INTEGRITY_FAILED",
    );
    assert!(
        error.to_string().contains(message),
        "unexpected inspection error: {error}"
    );
}

#[cfg(unix)]
fn symlink_file(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("create file symlink");
}

#[cfg(windows)]
fn symlink_file(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_file(target, link).expect("create file reparse point");
}

#[cfg(unix)]
fn symlink_directory(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).expect("create directory symlink");
}

#[cfg(windows)]
fn symlink_directory(target: &Path, link: &Path) {
    std::os::windows::fs::symlink_dir(target, link).expect("create directory reparse point");
}

fn expect_error_code<T>(result: migration_stream::Result<T>, expected: &str) -> KernelError {
    match result {
        Ok(_) => panic!("expected {expected} failure"),
        Err(error) => {
            assert_eq!(error.code(), expected, "unexpected error: {error}");
            error
        }
    }
}

#[test]
fn read_only_wrappers_do_not_create_a_missing_workspace_or_database() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let workspace = temporary.path().join("absent-workspace");

    let error = expect_error_code(
        inspect_mapping(&workspace, "missing.mapping", "1.0.0"),
        "NOT_FOUND",
    );

    assert!(error.to_string().contains("workspace mapping database"));
    inspect_profile(&workspace, PROFILE_ID, PROFILE_VERSION)
        .expect_err("profile inspection must not initialize a missing workspace");
    validation_status(&workspace, "missing-validation-run")
        .expect_err("validation status must not initialize a missing workspace");
    assert!(
        !workspace.exists(),
        "inspection created the missing workspace"
    );
    assert!(
        !workspace.join("state.sqlite3").exists(),
        "inspection created a database"
    );
    assert!(
        !workspace.join(".migration-stream.lock").exists(),
        "inspection created a writer lock file"
    );
}

#[test]
fn read_only_wrappers_do_not_migrate_an_older_schema() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let workspace = temporary.path().join("legacy-workspace");
    fs::create_dir(&workspace).expect("create legacy workspace");
    let database_path = workspace.join("state.sqlite3");
    let connection = Connection::open(&database_path).expect("create legacy database");
    connection
        .execute_batch(
            "CREATE TABLE legacy_marker (value TEXT NOT NULL);
             INSERT INTO legacy_marker (value) VALUES ('preserve');
             PRAGMA user_version = 4;",
        )
        .expect("create legacy schema marker");
    drop(connection);
    let before = fs::read(&database_path).expect("read legacy database before inspection");

    let error = expect_error_code(
        inspect_mapping(&workspace, "missing.mapping", "1.0.0"),
        "CONFLICT",
    );

    let expected_schema_conflict = format!("schema version {SCHEMA_VERSION}; found 4");
    assert!(error.to_string().contains(&expected_schema_conflict));
    let profile_error = expect_error_code(
        inspect_profile(&workspace, PROFILE_ID, PROFILE_VERSION),
        "CONFLICT",
    );
    assert!(
        profile_error
            .to_string()
            .contains(&expected_schema_conflict)
    );
    let validation_error = expect_error_code(
        validation_status(&workspace, "missing-validation-run"),
        "CONFLICT",
    );
    assert!(
        validation_error
            .to_string()
            .contains(&expected_schema_conflict)
    );
    assert_eq!(
        fs::read(&database_path).expect("read legacy database after inspection"),
        before,
        "inspection changed legacy database bytes"
    );
    let verification = Connection::open(&database_path).expect("reopen legacy database");
    let schema_version: i64 = verification
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("read legacy schema version");
    let marker: String = verification
        .query_row("SELECT value FROM legacy_marker", [], |row| row.get(0))
        .expect("read legacy marker");
    assert_eq!(schema_version, 4);
    assert_eq!(marker, "preserve");
    assert!(
        !workspace.join(".migration-stream.lock").exists(),
        "inspection created a writer lock file"
    );
}

#[test]
fn read_only_wrappers_reject_substituted_workspace_and_database_links() {
    let fixture = prepare_registered_mapping();
    let workspace_alias = fixture.temporary.path().join("workspace-alias");
    symlink_directory(&fixture.workspace, &workspace_alias);
    expect_error_code(
        inspect_mapping(&workspace_alias, MAPPING_ID, MAPPING_VERSION),
        "INTEGRITY_FAILED",
    );
    expect_error_code(
        inspect_profile(&workspace_alias, PROFILE_ID, PROFILE_VERSION),
        "INTEGRITY_FAILED",
    );
    expect_error_code(
        validation_status(&workspace_alias, "missing-validation-run"),
        "INTEGRITY_FAILED",
    );

    let database = fixture.workspace.join("state.sqlite3");
    let real_database = fixture.workspace.join("authoritative-state.sqlite3");
    fs::rename(&database, &real_database).expect("move authoritative database for link proof");
    symlink_file(&real_database, &database);
    expect_error_code(
        inspect_mapping(&fixture.workspace, MAPPING_ID, MAPPING_VERSION),
        "INTEGRITY_FAILED",
    );
    expect_error_code(
        inspect_profile(&fixture.workspace, PROFILE_ID, PROFILE_VERSION),
        "INTEGRITY_FAILED",
    );
    expect_error_code(
        validation_status(&fixture.workspace, "missing-validation-run"),
        "INTEGRITY_FAILED",
    );
}

#[test]
fn inspect_does_not_acquire_or_block_on_the_writer_lock() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let workspace = temporary.path().join("initialized-workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let lock_path = workspace.join(".migration-stream.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open writer lock file");
    lock.try_lock_exclusive().expect("hold writer lock");

    let error = expect_error_code(
        inspect_mapping(&workspace, "missing.mapping", "1.0.0"),
        "MAPPING_INVALID",
    );

    assert!(error.to_string().contains("unknown mapping"));
    assert!(matches!(error, KernelError::MappingInvalid(_)));
    FileExt::unlock(&lock).expect("release writer lock");
}

#[test]
fn inspect_never_creates_state_inside_an_existing_empty_directory() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let workspace = temporary.path().join("empty-workspace");
    fs::create_dir(&workspace).expect("create empty workspace");

    expect_error_code(
        inspect_mapping(&workspace, "missing.mapping", "1.0.0"),
        "NOT_FOUND",
    );

    let entries = fs::read_dir(&workspace)
        .expect("read empty workspace")
        .collect::<std::io::Result<Vec<_>>>()
        .expect("collect empty workspace entries");
    assert!(entries.is_empty(), "inspection created workspace state");
}

#[test]
fn mapping_document_is_rejected_from_metadata_before_an_oversize_allocation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let mapping = temporary.path().join("oversize-mapping.json");
    create_sparse_file(&mapping, MAPPING_LIMIT + 1);

    let error = expect_error_code(
        validate_mapping(
            &workspace,
            "not-reached",
            &project_path("schemas/source-mapping-version.schema.json"),
            &project_path("schemas/migration-object-profile.schema.json"),
            &mapping,
        ),
        "MAPPING_INVALID",
    );

    assert!(error.to_string().contains("mapping document bytes 2097153"));
    assert!(error.to_string().contains("exceed limit 2097152"));
}

#[test]
fn caller_selected_mapping_schema_is_rejected_before_an_oversize_allocation() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let mapping = temporary.path().join("mapping.json");
    fs::write(&mapping, b"{}").expect("write bounded mapping document");
    let schema = temporary.path().join("oversize-mapping-schema.json");
    create_sparse_file(&schema, MAPPING_SCHEMA_LIMIT + 1);

    let error = expect_error_code(
        validate_mapping(
            &workspace,
            "not-reached",
            &schema,
            &project_path("schemas/migration-object-profile.schema.json"),
            &mapping,
        ),
        "MAPPING_INVALID",
    );

    assert!(error.to_string().contains("mapping schema bytes 4194305"));
    assert!(error.to_string().contains("exceed limit 4194304"));
}

#[cfg(any(unix, windows))]
#[test]
fn mapping_document_symlink_or_reparse_point_is_rejected_without_following_it() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let target = temporary.path().join("mapping-target.json");
    fs::write(&target, b"{}").expect("write mapping target");
    let mapping = temporary.path().join("mapping-link.json");
    symlink_file(&target, &mapping);

    let error = expect_error_code(
        validate_mapping(
            &workspace,
            "not-reached",
            &project_path("schemas/source-mapping-version.schema.json"),
            &project_path("schemas/migration-object-profile.schema.json"),
            &mapping,
        ),
        "MAPPING_INVALID",
    );

    assert!(error.to_string().contains("regular non-link file"));
    assert_eq!(
        fs::read(&target).expect("read untouched mapping target"),
        b"{}"
    );
}

#[cfg(any(unix, windows))]
#[test]
fn mapping_schema_symlink_or_reparse_point_is_rejected_without_following_it() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let mapping = temporary.path().join("mapping.json");
    fs::write(&mapping, b"{}").expect("write bounded mapping document");
    let target = temporary.path().join("schema-target.json");
    fs::write(&target, b"{}").expect("write schema target");
    let schema = temporary.path().join("schema-link.json");
    symlink_file(&target, &schema);

    let error = expect_error_code(
        validate_mapping(
            &workspace,
            "not-reached",
            &schema,
            &project_path("schemas/migration-object-profile.schema.json"),
            &mapping,
        ),
        "MAPPING_INVALID",
    );

    assert!(error.to_string().contains("regular non-link file"));
    assert_eq!(
        fs::read(&target).expect("read untouched schema target"),
        b"{}"
    );
}

#[test]
fn inspect_replays_exact_document_coverage_and_accepted_provenance() {
    let fixture = prepare_registered_mapping();

    let view = inspect_mapping(&fixture.workspace, MAPPING_ID, MAPPING_VERSION)
        .expect("inspect exact immutable mapping evidence");

    assert_eq!(view.mapping_id, MAPPING_ID);
    assert_eq!(view.mapping_version, MAPPING_VERSION);
    assert_eq!(view.mapping_hash, fixture.mapping_hash);
    assert_eq!(view.source.package_sha256, fixture.source_hash);
    assert_eq!(view.validated_job_id, fixture.job_id);
    assert_eq!(view.coverage.mapping_hash, view.mapping_hash);
    assert_eq!(view.coverage.profile, view.profile);
    assert_eq!(view.coverage.source, view.source);
}

#[test]
fn inspect_rejects_document_bytes_without_their_canonical_hash() {
    let fixture = prepare_registered_mapping();
    let mut document = read_document(&fixture);
    document["author"] = json!("deliberately tampered without rehash");
    replace_document(&fixture, &document, false);

    expect_inspection_integrity(&fixture, "document canonical hash mismatch");
}

#[test]
fn inspect_rejects_a_rehashed_document_identity_change() {
    let fixture = prepare_registered_mapping();
    let mut document = read_document(&fixture);
    document["profile"]["profileId"] = json!("synthetic.customer.rehashed-tamper");
    replace_document(&fixture, &document, true);

    expect_inspection_integrity(
        &fixture,
        "document identities differ from authoritative mapping columns",
    );
}

#[test]
fn inspect_rejects_rehashed_mapping_rows_not_bound_to_the_valid_attempt() {
    let fixture = prepare_registered_mapping();
    let mut document = read_document(&fixture);
    document["author"] = json!("rehashed row without immutable attempt");
    let mapping_hash = replace_document(&fixture, &document, true);
    let mut coverage = read_coverage(&fixture);
    coverage["mappingHash"] = json!(mapping_hash);
    replace_coverage(&fixture, &coverage, true);

    expect_inspection_integrity(
        &fixture,
        "immutable VALID registration attempt does not bind the mapping version",
    );
}

#[test]
fn inspect_replays_the_exact_profile_approval_provenance() {
    let fixture = prepare_registered_mapping();
    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open authoritative state for deliberate tamper");
    with_immutability_guard_disabled(
        &connection,
        "profile_approvals_no_update",
        PROFILE_APPROVALS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE profile_approvals SET reason = reason || ' [tampered]'
                     WHERE profile_hash = (SELECT profile_hash FROM mapping_versions
                                           WHERE mapping_id = ?1 AND mapping_version = ?2)",
                    params![MAPPING_ID, MAPPING_VERSION],
                )
                .expect("tamper exact profile approval provenance");
        },
    );

    expect_inspection_integrity(&fixture, "profile provenance replay failed");
}

#[test]
fn inspect_rejects_coverage_bytes_without_their_canonical_hash() {
    let fixture = prepare_registered_mapping();
    let mut coverage = read_coverage(&fixture);
    coverage["mappedCanonicalFields"] = json!(["category"]);
    replace_coverage(&fixture, &coverage, false);

    expect_inspection_integrity(&fixture, "coverage canonical hash mismatch");
}

#[test]
fn inspect_preflights_oversized_stored_coverage_before_loading_text() {
    let fixture = prepare_registered_mapping();
    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open mapping evidence for oversized-blob tamper");
    with_immutability_guard_disabled(
        &connection,
        "mapping_versions_no_update",
        MAPPING_VERSIONS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE mapping_versions
                     SET coverage_json = CAST(zeroblob(8388609) AS TEXT)
                     WHERE mapping_id = ?1 AND mapping_version = ?2",
                    params![MAPPING_ID, MAPPING_VERSION],
                )
                .expect("install oversized stored coverage evidence");
        },
    );

    expect_inspection_integrity(&fixture, "stored document/coverage bytes");
}

#[test]
fn inspect_rejects_a_rehashed_coverage_identity_change() {
    let fixture = prepare_registered_mapping();
    let mut coverage = read_coverage(&fixture);
    coverage["validatedJobId"] = json!("rehashed-provenance-tamper");
    replace_coverage(&fixture, &coverage, true);

    expect_inspection_integrity(
        &fixture,
        "coverage identities differ from its exact mapping and registration job",
    );
}

#[test]
fn inspect_rejects_rehashed_coverage_that_differs_from_deterministic_replay() {
    let fixture = prepare_registered_mapping();
    let mut coverage = read_coverage(&fixture);
    coverage["unmappedSourceColumns"] = json!([{
        "sourceFile": "customer_accounts.csv",
        "sourceColumn": "migrationPartnerId",
        "sourceColumnOrdinal": 0
    }]);
    replace_coverage(&fixture, &coverage, true);

    expect_inspection_integrity(
        &fixture,
        "coverage differs from deterministic profile/source replay",
    );
}

#[test]
fn inspect_rejects_governed_coverage_above_the_source_column_limit() {
    let fixture = prepare_registered_mapping();
    let headers = (0..=4_096)
        .map(|ordinal| format!("column_{ordinal:04}"))
        .collect::<Vec<_>>();
    Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open state for deliberate wide-header tamper")
        .execute(
            "UPDATE csv_files SET header_json = ?1 WHERE job_id = ?2 AND path = ?3",
            params![
                canonical_json_string(&json!(headers)).expect("canonical wide header"),
                fixture.job_id,
                "customer_accounts.csv"
            ],
        )
        .expect("install wide accepted-header evidence");

    expect_inspection_integrity(&fixture, "governed mapping limit is 4096");
}

#[test]
fn inspect_rejects_a_registration_job_that_is_no_longer_complete() {
    let fixture = prepare_registered_mapping();
    Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open authoritative state for deliberate tamper")
        .execute(
            "UPDATE jobs SET state = 'INTERRUPTED', error_code = 'DELIBERATE_TAMPER'
             WHERE id = ?1",
            [&fixture.job_id],
        )
        .expect("tamper registration job state");

    expect_inspection_integrity(
        &fixture,
        "registration job is not COMPLETE with the same accepted source hash",
    );
}

#[test]
fn inspect_rejects_changed_accepted_source_hash_provenance() {
    let fixture = prepare_registered_mapping();
    let changed_hash = "0".repeat(64);
    Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open authoritative state for deliberate tamper")
        .execute(
            "UPDATE jobs SET source_hash = ?1, stored_archive = ?2 WHERE id = ?3",
            params![
                changed_hash,
                format!("packages/{changed_hash}.zip"),
                fixture.job_id
            ],
        )
        .expect("tamper accepted source hash provenance");

    expect_inspection_integrity(
        &fixture,
        "registration job is not COMPLETE with the same accepted source hash",
    );
}

#[test]
fn inspect_rejects_a_tombstone_accepted_archive_path() {
    let fixture = prepare_registered_mapping();
    Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open authoritative state for deliberate tamper")
        .execute(
            "UPDATE jobs SET stored_archive = 'packages/deliberate.tombstone' WHERE id = ?1",
            [&fixture.job_id],
        )
        .expect("tamper accepted archive provenance");

    expect_inspection_integrity(&fixture, "accepted archive is quarantined");
}

#[test]
fn inspect_rejects_fully_rehashed_nonaccepted_source_path_evidence() {
    let fixture = prepare_registered_mapping();
    let mut document = read_document(&fixture);
    document["source"]["sourceFiles"] = json!(["absent.csv"]);
    let mapping_hash = replace_document(&fixture, &document, true);
    let mut coverage = read_coverage(&fixture);
    coverage["source"] = document["source"].clone();
    coverage["mappingHash"] = json!(mapping_hash);
    replace_coverage(&fixture, &coverage, true);
    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open authoritative state for deliberate attempt tamper");
    with_immutability_guard_disabled(
        &connection,
        "mapping_validation_attempts_no_update",
        MAPPING_ATTEMPTS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE mapping_validation_attempts SET mapping_hash = ?1
                     WHERE id = (SELECT attempt_id FROM mapping_versions
                                 WHERE mapping_id = ?2 AND mapping_version = ?3)",
                    params![mapping_hash, MAPPING_ID, MAPPING_VERSION],
                )
                .expect("rehash attempt anchor to exercise accepted-source replay");
        },
    );

    expect_inspection_integrity(
        &fixture,
        "source file \"absent.csv\" is not an accepted CSV",
    );
}

#[test]
fn inspect_rejects_fully_rehashed_tombstone_source_evidence() {
    let fixture = prepare_registered_mapping();
    let mut document = read_document(&fixture);
    document["source"]["sourceFiles"] = json!(["quarantine/inert.tombstone/data.csv"]);
    replace_document(&fixture, &document, true);

    expect_inspection_integrity(&fixture, "document names a quarantined source");
}

use std::{
    fs,
    path::{Path, PathBuf},
};

use migration_stream::{
    KernelError,
    canonical::{canonical_hash, canonical_json_string},
    intake::ingest_package,
    mapping::validate_mapping,
    policy::IntakePolicy,
    profile_registry::{
        ProfileAuthority, ProfileLifecycleStatus, ProfileVersionView, RegistryLimits,
        approve_profile, register_profile,
    },
    read_model::{IssueFilters, PageRequest, ReadService},
    state::{integrity_check, recover_workspace},
    validation::{
        ValidationControl, ValidationPolicy, ValidationRunState, run_validation,
        run_validation_with_control, validation_status,
    },
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tempfile::TempDir;

#[path = "support/object_workspace.rs"]
mod object_workspace_support;

const PROFILE_ID: &str = "synthetic.customer.readiness";
const PROFILE_VERSION: &str = "1.0.0";
const MAPPING_ID: &str = "synthetic.customer.valid";
const MAPPING_VERSION: &str = "1.0.0";
const PROFILE_APPROVALS_UPDATE_GUARD_SQL: &str = "CREATE TRIGGER profile_approvals_no_update BEFORE UPDATE ON profile_approvals BEGIN SELECT RAISE(ABORT, 'profile approval history is immutable'); END";
const MAPPING_VERSIONS_UPDATE_GUARD_SQL: &str = "CREATE TRIGGER mapping_versions_no_update BEFORE UPDATE ON mapping_versions BEGIN SELECT RAISE(ABORT, 'mapping version history is immutable'); END";

struct GovernedFixture {
    _temporary: TempDir,
    workspace: PathBuf,
    job_id: String,
    profile_hash: String,
}

fn project_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn intake_policy() -> IntakePolicy {
    IntakePolicy {
        minimum_free_space_bytes: 0,
        transaction_batch_rows: 2,
        ..IntakePolicy::default()
    }
}

fn finalize_mapping(template: &Path, destination: &Path, profile_hash: &str) {
    let document = fs::read_to_string(template).expect("read mapping template");
    let placeholders = document.matches("__PROFILE_SHA256__").count();
    assert_eq!(
        placeholders, 1,
        "mapping fixture must have one profile token"
    );
    fs::write(
        destination,
        document.replace("__PROFILE_SHA256__", profile_hash),
    )
    .expect("write finalized mapping");
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

fn register_draft(workspace: &Path) -> ProfileVersionView {
    register_profile(
        workspace,
        &project_path("schemas/migration-object-profile.schema.json"),
        &project_path("fixtures/wp003/synthetic-profile.reviewed.json"),
        &RegistryLimits::default(),
    )
    .expect("register synthetic reviewed profile")
}

fn approve_registered_profile(workspace: &Path, profile_hash: &str) -> ProfileVersionView {
    approve_profile(
        workspace,
        PROFILE_ID,
        PROFILE_VERSION,
        profile_hash,
        "WP-003 integration test",
        "Authorize this exact synthetic profile for local mapping and validation only",
        true,
        &RegistryLimits::default(),
    )
    .expect("approve exact registered profile")
}

fn prepare_governed_fixture() -> GovernedFixture {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let intake = ingest_package(
        &project_path("fixtures/packages/valid.zip"),
        &workspace,
        &intake_policy(),
    )
    .expect("ingest synthetic source package");
    let draft = register_draft(&workspace);
    let approved = approve_registered_profile(&workspace, &draft.profile_hash);
    assert_eq!(approved.lifecycle_status, ProfileLifecycleStatus::Approved);
    assert_eq!(approved.authority, ProfileAuthority::LocalMappingValidation);

    let mapping_path = temporary.path().join("valid-mapping.json");
    finalize_mapping(
        &project_path("fixtures/wp003/valid-mapping.template.json"),
        &mapping_path,
        &approved.profile_hash,
    );
    validate_mapping(
        &workspace,
        &intake.job_id,
        &project_path("schemas/source-mapping-version.schema.json"),
        &project_path("schemas/migration-object-profile.schema.json"),
        &mapping_path,
    )
    .expect("register valid mapping");

    GovernedFixture {
        _temporary: temporary,
        workspace,
        job_id: intake.job_id,
        profile_hash: approved.profile_hash,
    }
}

fn latest_validation_run_id(workspace: &Path) -> String {
    let connection =
        Connection::open(workspace.join("state.sqlite3")).expect("open authoritative state");
    connection
        .query_row(
            "SELECT id FROM validation_runs ORDER BY rowid DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("latest validation run")
}

fn validation_event_states(workspace: &Path, run_id: &str) -> Vec<String> {
    let connection =
        Connection::open(workspace.join("state.sqlite3")).expect("open authoritative state");
    let mut statement = connection
        .prepare("SELECT state FROM validation_events WHERE run_id = ?1 ORDER BY sequence")
        .expect("prepare validation-event query");
    statement
        .query_map([run_id], |row| row.get(0))
        .expect("query validation events")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read validation events")
}

#[test]
fn validation_rejects_rehashed_mapping_not_bound_to_its_valid_attempt() {
    let fixture = prepare_governed_fixture();
    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open mapping evidence for deliberate tamper");
    let (document_json, coverage_json): (String, String) = connection
        .query_row(
            "SELECT document_json, coverage_json FROM mapping_versions
             WHERE mapping_id = ?1 AND mapping_version = ?2",
            params![MAPPING_ID, MAPPING_VERSION],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("load exact mapping evidence");
    let mut document: Value = serde_json::from_str(&document_json).expect("parse mapping document");
    document["author"] = json!("rehashed runtime-validation tamper");
    let changed_mapping_hash = canonical_hash(&document).expect("hash changed mapping document");
    let mut coverage: Value = serde_json::from_str(&coverage_json).expect("parse mapping coverage");
    coverage["mappingHash"] = json!(changed_mapping_hash);
    let changed_coverage_hash = canonical_hash(&coverage).expect("hash changed mapping coverage");
    with_immutability_guard_disabled(
        &connection,
        "mapping_versions_no_update",
        MAPPING_VERSIONS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE mapping_versions
                     SET mapping_hash = ?1, document_json = ?2, coverage_json = ?3, coverage_hash = ?4
                     WHERE mapping_id = ?5 AND mapping_version = ?6",
                    params![
                        changed_mapping_hash,
                        canonical_json_string(&document).expect("canonical mapping document"),
                        canonical_json_string(&coverage).expect("canonical mapping coverage"),
                        changed_coverage_hash,
                        MAPPING_ID,
                        MAPPING_VERSION
                    ],
                )
                .expect("rehash mapping row without changing its immutable attempt");
        },
    );
    drop(connection);

    let policy =
        ValidationPolicy::from_path(&project_path("fixtures/wp003/validation-policy.json"))
            .expect("load validation policy");
    let error = expect_error_code(
        run_validation(
            &fixture.workspace,
            &fixture.job_id,
            MAPPING_ID,
            MAPPING_VERSION,
            &project_path("schemas/migration-object-profile.schema.json"),
            &policy,
        ),
        "INTEGRITY_FAILED",
    );
    assert!(
        error
            .to_string()
            .contains("immutable VALID registration attempt")
    );
    let run_count: i64 = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("reopen authoritative state")
        .query_row("SELECT COUNT(*) FROM validation_runs", [], |row| row.get(0))
        .expect("count validation attempts");
    assert_eq!(run_count, 0, "mapping tamper created a validation run");
}

fn issue_outcomes(workspace: &Path, run_id: &str) -> Vec<(u64, String)> {
    let connection =
        Connection::open(workspace.join("state.sqlite3")).expect("open authoritative state");
    let mut statement = connection
        .prepare(
            "SELECT source_ordinal, issue_code
             FROM validation_issues
             WHERE run_id = ?1
             ORDER BY source_ordinal, issue_code",
        )
        .expect("prepare issue query");
    statement
        .query_map([run_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query issues")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("read issues")
}

#[test]
#[allow(clippy::too_many_lines)]
fn authority_mapping_and_validation_are_bounded_and_deterministic() {
    let temporary = tempfile::tempdir_in(project_path("target"))
        .expect("link-safe temporary directory beneath the build target");
    let workspace = temporary.path().join("workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let first_job = ingest_package(
        &project_path("fixtures/packages/valid.zip"),
        &workspace,
        &intake_policy(),
    )
    .expect("ingest first synthetic source package");

    let draft = register_draft(&workspace);
    assert_eq!(
        draft.lifecycle_status,
        ProfileLifecycleStatus::ReviewedDraft
    );
    assert_eq!(draft.authority, ProfileAuthority::None);
    assert!(!draft.can_start_new_runs);

    let valid_mapping_path = temporary.path().join("valid-mapping.json");
    finalize_mapping(
        &project_path("fixtures/wp003/valid-mapping.template.json"),
        &valid_mapping_path,
        &draft.profile_hash,
    );
    let draft_error = expect_error_code(
        validate_mapping(
            &workspace,
            &first_job.job_id,
            &project_path("schemas/source-mapping-version.schema.json"),
            &project_path("schemas/migration-object-profile.schema.json"),
            &valid_mapping_path,
        ),
        "PROFILE_NOT_APPROVED",
    );
    assert!(draft_error.to_string().contains("REVIEWED_DRAFT"));

    let approved = approve_registered_profile(&workspace, &draft.profile_hash);
    assert_eq!(approved.profile_hash, draft.profile_hash);
    assert_eq!(approved.lifecycle_status, ProfileLifecycleStatus::Approved);
    assert_eq!(approved.authority, ProfileAuthority::LocalMappingValidation);
    assert!(approved.can_start_new_runs);

    let mapping = validate_mapping(
        &workspace,
        &first_job.job_id,
        &project_path("schemas/source-mapping-version.schema.json"),
        &project_path("schemas/migration-object-profile.schema.json"),
        &valid_mapping_path,
    )
    .expect("validate finalized mapping under exact authority");
    assert_eq!(mapping.state, "VALID");
    assert_eq!(mapping.coverage.mapped_canonical_fields.len(), 5);
    assert_eq!(mapping.coverage.defaulted_canonical_fields.len(), 1);
    assert!(mapping.coverage.unmapped_source_columns.is_empty());
    assert!(
        mapping
            .coverage
            .required_canonical_fields_without_coverage
            .is_empty()
    );
    assert!(mapping.coverage.invalid_bindings.is_empty());

    let policy =
        ValidationPolicy::from_path(&project_path("fixtures/wp003/validation-policy.json"))
            .expect("load bounded validation policy");
    let first = run_validation(
        &workspace,
        &first_job.job_id,
        MAPPING_ID,
        MAPPING_VERSION,
        &project_path("schemas/migration-object-profile.schema.json"),
        &policy,
    )
    .expect("validate first accepted job");
    assert_eq!(first.result.rows_processed, 4);
    assert_eq!(first.result.issue_count, 5);
    assert_eq!(first.result.severity_counts.get("ERROR"), Some(&3));
    assert_eq!(first.result.severity_counts.get("WARNING"), Some(&2));
    assert_eq!(
        first
            .result
            .severity_counts
            .get("INFORMATION")
            .copied()
            .unwrap_or_default(),
        0
    );
    assert_eq!(
        first.result.rule_counts.get("SYN-CONDITIONAL-001"),
        Some(&1)
    );
    assert_eq!(first.result.rule_counts.get("SYN-PATTERN-001"), Some(&2));
    assert_eq!(first.result.rule_counts.get("SYN-UNIQUE-001"), Some(&2));
    assert_eq!(
        issue_outcomes(&workspace, &first.run_id),
        vec![
            (2, "PATTERN_MISMATCH".to_owned()),
            (3, "PATTERN_MISMATCH".to_owned()),
            (3, "UNIQUE_DUPLICATE".to_owned()),
            (4, "CONDITIONAL_REQUIRED_MISSING".to_owned()),
            (4, "UNIQUE_DUPLICATE".to_owned()),
        ]
    );
    let first_status = validation_status(&workspace, &first.run_id).expect("first run status");
    assert_eq!(first_status.state, ValidationRunState::Complete);
    assert_eq!(
        first_status.result_hash.as_deref(),
        Some(first.result_hash.as_str())
    );

    let second_job = ingest_package(
        &project_path("fixtures/packages/valid.zip"),
        &workspace,
        &intake_policy(),
    )
    .expect("ingest independent second job over identical accepted bytes");
    assert_ne!(first_job.job_id, second_job.job_id);
    assert_eq!(first_job.source_hash, second_job.source_hash);
    let second = run_validation(
        &workspace,
        &second_job.job_id,
        MAPPING_ID,
        MAPPING_VERSION,
        &project_path("schemas/migration-object-profile.schema.json"),
        &policy,
    )
    .expect("validate independent second job");
    assert_ne!(first.run_id, second.run_id);
    assert_eq!(first.identity_hash, second.identity_hash);
    assert_eq!(first.result_hash, second.result_hash);
    assert_eq!(first.result, second.result);
    assert_eq!(
        issue_outcomes(&workspace, &first.run_id),
        issue_outcomes(&workspace, &second.run_id)
    );
    let reads = ReadService::new(&workspace);
    let second_summary = reads
        .validation_summary(&second.run_id)
        .expect("read independent second-job validation summary");
    assert_eq!(second_summary.data.job_id, second_job.job_id);
    assert_eq!(second_summary.data.issue_count, 5);
    let second_issues = reads
        .issue_list(
            &second.run_id,
            &IssueFilters::default(),
            &PageRequest::default(),
        )
        .expect("read independent second-job issues");
    assert_eq!(second_issues.items.len(), 5);
    let second_row = reads
        .row_detail(&second.run_id, "customer_accounts.csv", 4)
        .expect("read independent second-job row detail");
    assert_eq!(second_row.data.job_id, second_job.job_id);
    let second_reconciliation = reads
        .reconciliation_summary(
            &second_job.job_id,
            Some(&second.run_id),
            &PageRequest::default(),
        )
        .expect("read independent second-job reconciliation");
    assert!(second_reconciliation.aggregate.closed);

    let invalid_mappings = [
        (
            "fixtures/wp003/invalid-conflicting-target.template.json",
            "MAPPING_INVALID",
            "conflicting bindings target",
        ),
        (
            "fixtures/wp003/invalid-missing-lookup-table.template.json",
            "MAPPING_INVALID",
            "references unknown table",
        ),
        (
            "fixtures/wp003/invalid-missing-required-coverage.template.json",
            "MAPPING_INVALID",
            "required canonical fields",
        ),
        (
            "fixtures/wp003/invalid-tombstone-source.template.json",
            "MAPPING_INVALID",
            "quarantine/live-partial",
        ),
        (
            "fixtures/wp003/invalid-unknown-source-column.template.json",
            "MAPPING_INVALID",
            "is not accepted",
        ),
        (
            "fixtures/wp003/invalid-unsupported-operation.template.json",
            "MAPPING_INVALID",
            "sql_expression",
        ),
    ];
    for (index, (relative, expected_code, expected_diagnostic)) in
        invalid_mappings.iter().enumerate()
    {
        let path = temporary.path().join(format!("invalid-{index}.json"));
        finalize_mapping(&project_path(relative), &path, &approved.profile_hash);
        let error = expect_error_code(
            validate_mapping(
                &workspace,
                &first_job.job_id,
                &project_path("schemas/source-mapping-version.schema.json"),
                &project_path("schemas/migration-object-profile.schema.json"),
                &path,
            ),
            expected_code,
        );
        assert!(
            error.to_string().contains(expected_diagnostic),
            "{relative} failed for the wrong reason: {error}"
        );
    }
    let hash_error = expect_error_code(
        validate_mapping(
            &workspace,
            &first_job.job_id,
            &project_path("schemas/source-mapping-version.schema.json"),
            &project_path("schemas/migration-object-profile.schema.json"),
            &project_path("fixtures/wp003/invalid-profile-hash.json"),
        ),
        "CONFLICT",
    );
    assert!(hash_error.to_string().contains("hash mismatch"));
}

#[test]
fn bounds_cancellation_and_recovery_preserve_explicit_terminal_history() {
    let fixture = prepare_governed_fixture();
    let mut bounded_policy =
        ValidationPolicy::from_path(&project_path("fixtures/wp003/validation-policy.json"))
            .expect("load bounded validation policy");
    bounded_policy.row_batch_size = 1;
    bounded_policy.maximum_issues = 1;

    expect_error_code(
        run_validation(
            &fixture.workspace,
            &fixture.job_id,
            MAPPING_ID,
            MAPPING_VERSION,
            &project_path("schemas/migration-object-profile.schema.json"),
            &bounded_policy,
        ),
        "VALIDATION_BOUND_EXCEEDED",
    );
    let bounded_run_id = latest_validation_run_id(&fixture.workspace);
    let bounded_status =
        validation_status(&fixture.workspace, &bounded_run_id).expect("bounded run status");
    assert_eq!(bounded_status.state, ValidationRunState::BoundExceeded);
    assert_eq!(bounded_status.rows_processed, 2);
    assert_eq!(bounded_status.issue_count, 1);
    assert!(bounded_status.result_hash.is_none());
    assert_eq!(
        bounded_status.error_code.as_deref(),
        Some("VALIDATION_BOUND_EXCEEDED")
    );
    assert_eq!(
        validation_event_states(&fixture.workspace, &bounded_run_id),
        vec!["RUNNING".to_owned(), "BOUND_EXCEEDED".to_owned()]
    );

    let policy =
        ValidationPolicy::from_path(&project_path("fixtures/wp003/validation-policy.json"))
            .expect("load cancellation validation policy");
    expect_error_code(
        run_validation_with_control(
            &fixture.workspace,
            &fixture.job_id,
            MAPPING_ID,
            MAPPING_VERSION,
            &project_path("schemas/migration-object-profile.schema.json"),
            &policy,
            &ValidationControl {
                cancel_file: None,
                cancel_after_batches: Some(1),
            },
        ),
        "COOPERATIVE_CANCELLED",
    );
    let interrupted_run_id = latest_validation_run_id(&fixture.workspace);
    assert_ne!(bounded_run_id, interrupted_run_id);
    let interrupted_status =
        validation_status(&fixture.workspace, &interrupted_run_id).expect("interrupted run status");
    assert_eq!(interrupted_status.state, ValidationRunState::Interrupted);
    assert_eq!(interrupted_status.rows_processed, 2);
    assert_eq!(interrupted_status.issue_count, 1);
    assert!(interrupted_status.result_hash.is_none());
    assert_eq!(
        interrupted_status.error_code.as_deref(),
        Some("COOPERATIVE_CANCELLED")
    );
    assert_eq!(
        validation_event_states(&fixture.workspace, &interrupted_run_id),
        vec!["RUNNING".to_owned(), "INTERRUPTED".to_owned()]
    );

    let first_recovery = recover_workspace(&fixture.workspace).expect("first recovery pass");
    let second_recovery = recover_workspace(&fixture.workspace).expect("second recovery pass");
    assert_eq!(first_recovery.recovered_validation_runs, 0);
    assert_eq!(second_recovery.recovered_validation_runs, 0);
    assert_eq!(
        validation_status(&fixture.workspace, &bounded_run_id)
            .expect("bounded status after repeated recovery")
            .state,
        ValidationRunState::BoundExceeded
    );
    assert_eq!(
        validation_status(&fixture.workspace, &interrupted_run_id)
            .expect("interrupted status after repeated recovery")
            .state,
        ValidationRunState::Interrupted
    );
    assert_eq!(
        validation_event_states(&fixture.workspace, &bounded_run_id).len(),
        2
    );
    assert_eq!(
        validation_event_states(&fixture.workspace, &interrupted_run_id).len(),
        2
    );

    let integrity = integrity_check(&fixture.workspace).expect("terminal-attempt integrity replay");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
    assert_eq!(integrity.checked_validation_runs, 2);
    assert_eq!(integrity.checked_validation_issues, 2);
}

#[test]
fn terminal_checkpoint_rejects_coherent_interrupted_history_shortening() {
    let fixture = prepare_governed_fixture();
    let mut policy =
        ValidationPolicy::from_path(&project_path("fixtures/wp003/validation-policy.json"))
            .expect("load cancellation validation policy");
    policy.row_batch_size = 2;
    expect_error_code(
        run_validation_with_control(
            &fixture.workspace,
            &fixture.job_id,
            MAPPING_ID,
            MAPPING_VERSION,
            &project_path("schemas/migration-object-profile.schema.json"),
            &policy,
            &ValidationControl {
                cancel_file: None,
                cancel_after_batches: Some(1),
            },
        ),
        "COOPERATIVE_CANCELLED",
    );
    let run_id = latest_validation_run_id(&fixture.workspace);
    let before = validation_status(&fixture.workspace, &run_id)
        .expect("sealed interrupted validation status");
    assert_eq!(before.state, ValidationRunState::Interrupted);
    assert_eq!(before.rows_processed, 2);
    assert_eq!(before.issue_count, 1);

    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open terminal validation evidence");
    let immutable_error = connection
        .execute(
            "UPDATE validation_runs SET terminal_hash = lower(hex(randomblob(32)))
             WHERE id = ?1",
            [&run_id],
        )
        .expect_err("terminal checkpoint update must be blocked");
    assert!(
        immutable_error
            .to_string()
            .contains("terminal checkpoint is immutable")
    );
    connection
        .execute("DELETE FROM validation_issues WHERE run_id = ?1", [&run_id])
        .expect("simulate shortened interrupted issue history");
    connection
        .execute(
            "UPDATE validation_runs SET rows_processed = 0, issue_count = 0 WHERE id = ?1",
            [&run_id],
        )
        .expect("simulate coherently shortened interrupted counters");
    drop(connection);

    let status_error = expect_error_code(
        validation_status(&fixture.workspace, &run_id),
        "INTEGRITY_FAILED",
    );
    assert!(
        status_error
            .to_string()
            .contains("terminal checkpoint differs")
    );
    let integrity = integrity_check(&fixture.workspace).expect("replay shortened terminal history");
    assert!(!integrity.ok);
    assert!(
        integrity
            .issues
            .join("\n")
            .contains("terminal checkpoint differs")
    );
}

#[test]
fn integrity_replay_detects_profile_mapping_and_issue_tampering() {
    let fixture = prepare_governed_fixture();
    let policy =
        ValidationPolicy::from_path(&project_path("fixtures/wp003/validation-policy.json"))
            .expect("load validation policy");
    let validation = run_validation(
        &fixture.workspace,
        &fixture.job_id,
        MAPPING_ID,
        MAPPING_VERSION,
        &project_path("schemas/migration-object-profile.schema.json"),
        &policy,
    )
    .expect("create complete validation evidence");

    let baseline = integrity_check(&fixture.workspace).expect("baseline integrity replay");
    assert!(baseline.ok, "baseline issues: {:?}", baseline.issues);
    assert_eq!(baseline.checked_profile_versions, 1);
    assert_eq!(baseline.checked_profile_approvals, 1);
    assert_eq!(baseline.checked_mappings, 1);
    assert_eq!(baseline.checked_validation_runs, 1);
    assert_eq!(baseline.checked_validation_issues, 5);

    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open state for tamper proof");
    let changed_approval = with_immutability_guard_disabled(
        &connection,
        "profile_approvals_no_update",
        PROFILE_APPROVALS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE profile_approvals SET reason = reason || ' [tampered]' WHERE profile_hash = ?1",
                    [&fixture.profile_hash],
                )
                .expect("tamper approval evidence")
        },
    );
    assert_eq!(changed_approval, 1);
    let changed_mapping = with_immutability_guard_disabled(
        &connection,
        "mapping_versions_no_update",
        MAPPING_VERSIONS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE mapping_versions SET coverage_json = '{}' WHERE mapping_id = ?1 AND mapping_version = ?2",
                    params![MAPPING_ID, MAPPING_VERSION],
                )
                .expect("tamper mapping coverage evidence")
        },
    );
    assert_eq!(changed_mapping, 1);
    let issue_id: String = connection
        .query_row(
            "SELECT issue_id FROM validation_issues WHERE run_id = ?1 ORDER BY issue_id LIMIT 1",
            [&validation.run_id],
            |row| row.get(0),
        )
        .expect("select issue evidence");
    let changed_issue = connection
        .execute(
            "UPDATE validation_issues SET display_message = display_message || ' [tampered]'
             WHERE run_id = ?1 AND issue_id = ?2",
            params![validation.run_id, issue_id],
        )
        .expect("tamper validation issue evidence");
    assert_eq!(changed_issue, 1);
    drop(connection);

    let replay = integrity_check(&fixture.workspace).expect("tampered integrity replay");
    assert!(!replay.ok);
    let diagnostics = replay.issues.join("\n").to_ascii_lowercase();
    assert!(diagnostics.contains("approval hash is invalid"));
    assert!(diagnostics.contains("coverage canonical hash mismatch"));
    assert!(diagnostics.contains("validation issue"));
    assert!(diagnostics.contains("hash mismatch"));
}

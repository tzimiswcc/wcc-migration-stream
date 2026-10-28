use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use jsonschema::PatternOptions;
use migration_stream::{
    KernelError,
    canonical::{canonical_hash, canonical_json_string, encode_cursor},
    intake::ingest_package,
    mapping::validate_mapping,
    policy::IntakePolicy,
    profile_registry::{RegistryLimits, approve_profile, register_profile},
    profiling::{ProfilingPolicy, run_profile},
    read_model::{
        CleanupFilters, IssueFilters, IssueSeverity, MAXIMUM_PAGE_SIZE, PageRequest, ReadService,
    },
    validation::{ValidationPolicy, run_validation},
};
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::{Value, json};
use tempfile::TempDir;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

#[path = "support/object_workspace.rs"]
mod object_workspace_support;

const PROFILE_ID: &str = "synthetic.customer.readiness";
const PROFILE_VERSION: &str = "1.0.0";
const MAPPING_ID: &str = "synthetic.customer.valid";
const MAPPING_VERSION: &str = "1.0.0";
const SOURCE_FILE: &str = "customer_accounts.csv";
const PROFILE_APPROVALS_UPDATE_GUARD_SQL: &str = "CREATE TRIGGER profile_approvals_no_update BEFORE UPDATE ON profile_approvals BEGIN SELECT RAISE(ABORT, 'profile approval history is immutable'); END";
const PROFILE_LIFECYCLE_UPDATE_GUARD_SQL: &str = "CREATE TRIGGER profile_lifecycle_events_no_update BEFORE UPDATE ON profile_lifecycle_events BEGIN SELECT RAISE(ABORT, 'profile lifecycle history is immutable'); END";
const MAPPING_VERSIONS_UPDATE_GUARD_SQL: &str = "CREATE TRIGGER mapping_versions_no_update BEFORE UPDATE ON mapping_versions BEGIN SELECT RAISE(ABORT, 'mapping version history is immutable'); END";

struct GovernedReadFixture {
    _temporary: TempDir,
    workspace: PathBuf,
    job_id: String,
    profile_hash: String,
    mapping_hash: String,
    profiling_run_id: String,
    validation_run_id: String,
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

fn intake_policy() -> IntakePolicy {
    IntakePolicy {
        minimum_free_space_bytes: 0,
        transaction_batch_rows: 2,
        ..IntakePolicy::default()
    }
}

fn profiling_policy() -> ProfilingPolicy {
    ProfilingPolicy {
        schema_version: 1,
        columns_per_pass: 2,
        row_batch_size: 2,
        maximum_profiled_files: 4,
        maximum_profiled_columns: 32,
        maximum_exact_distinct_values_per_column: 32,
        maximum_exact_distinct_bytes_per_column: 4_096,
        maximum_exact_distinct_values_per_pass: 64,
        maximum_exact_distinct_bytes_per_pass: 8_192,
        maximum_samples_per_column: 3,
        maximum_sample_value_bytes: 128,
        oversized_field_bytes: 1_024,
        duplicate_keys: Vec::new(),
    }
}

fn finalize_mapping(template: &Path, destination: &Path, profile_hash: &str) {
    let document = fs::read_to_string(template).expect("read mapping template");
    assert_eq!(document.matches("__PROFILE_SHA256__").count(), 1);
    fs::write(
        destination,
        document.replace("__PROFILE_SHA256__", profile_hash),
    )
    .expect("write finalized mapping");
}

fn write_two_file_package(path: &Path) {
    let file = fs::File::create(path).expect("create two-file package");
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for name in ["a.csv", "b.csv"] {
        writer
            .start_file(name, options)
            .expect("start synthetic CSV entry");
        writer
            .write_all(b"id,name\n1,Synthetic\n")
            .expect("write synthetic CSV entry");
    }
    writer.finish().expect("finish two-file package");
}

fn prepare_governed_fixture() -> GovernedReadFixture {
    prepare_governed_fixture_with_profile(|_| {})
}

fn prepare_governed_fixture_with_profile(
    mutate_profile: impl FnOnce(&mut Value),
) -> GovernedReadFixture {
    // SQLite's NOFOLLOW flag rejects macOS's /var -> /private/var temporary
    // directory alias. Keep the fixture beneath the real checkout path so this
    // test exercises the same link-safe read flags as production.
    let temporary = tempfile::tempdir_in(project_path("target"))
        .expect("link-safe temporary directory beneath the build target");
    let workspace = temporary.path().join("workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let intake = ingest_package(
        &project_path("fixtures/packages/valid.zip"),
        &workspace,
        &intake_policy(),
    )
    .expect("ingest synthetic source package");
    let mut profile: Value = serde_json::from_slice(
        &fs::read(project_path(
            "fixtures/wp003/synthetic-profile.reviewed.json",
        ))
        .expect("read synthetic profile fixture"),
    )
    .expect("parse synthetic profile fixture");
    mutate_profile(&mut profile);
    let profile_path = temporary.path().join("synthetic-profile.reviewed.json");
    fs::write(
        &profile_path,
        serde_json::to_vec_pretty(&profile).expect("serialize synthetic profile fixture"),
    )
    .expect("write synthetic profile fixture");
    let registered = register_profile(
        &workspace,
        &project_path("schemas/migration-object-profile.schema.json"),
        &profile_path,
        &RegistryLimits::default(),
    )
    .expect("register synthetic reviewed profile");
    let approved = approve_profile(
        &workspace,
        PROFILE_ID,
        PROFILE_VERSION,
        &registered.profile_hash,
        "WP-003 read-contract proof",
        "Authorize this exact synthetic profile for local mapping and validation only",
        true,
        &RegistryLimits::default(),
    )
    .expect("approve exact synthetic profile");
    let mapping_path = temporary.path().join("valid-mapping.json");
    finalize_mapping(
        &project_path("fixtures/wp003/valid-mapping.template.json"),
        &mapping_path,
        &approved.profile_hash,
    );
    let mapping = validate_mapping(
        &workspace,
        &intake.job_id,
        &project_path("schemas/source-mapping-version.schema.json"),
        &project_path("schemas/migration-object-profile.schema.json"),
        &mapping_path,
    )
    .expect("validate exact synthetic mapping");
    let profiling = run_profile(&workspace, &intake.job_id, &profiling_policy())
        .expect("profile accepted synthetic rows");
    let validation_policy =
        ValidationPolicy::from_path(&project_path("fixtures/wp003/validation-policy.json"))
            .expect("load bounded validation policy");
    let validation = run_validation(
        &workspace,
        &intake.job_id,
        MAPPING_ID,
        MAPPING_VERSION,
        &project_path("schemas/migration-object-profile.schema.json"),
        &validation_policy,
    )
    .expect("validate synthetic governed fixture");

    GovernedReadFixture {
        _temporary: temporary,
        workspace,
        job_id: intake.job_id,
        profile_hash: approved.profile_hash,
        mapping_hash: mapping.mapping_hash,
        profiling_run_id: profiling.run_id,
        validation_run_id: validation.run_id,
    }
}

fn assert_contract<T: Serialize>(response: &T, schema_relative_path: &str) -> Value {
    let mut document = serde_json::to_value(response).expect("serialize read response");
    let schema: Value = serde_json::from_slice(
        &fs::read(project_path(schema_relative_path)).expect("read response schema"),
    )
    .expect("parse response schema");
    jsonschema::draft202012::meta::validate(&schema).expect("valid response JSON Schema");
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .with_pattern_options(PatternOptions::regex())
        .build(&schema)
        .expect("compile response JSON Schema");
    let errors = validator
        .iter_errors(&document)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "{schema_relative_path} rejected its response:\n{}\nresponse: {document}",
        errors.join("\n")
    );

    let reported_hash = document["canonical_hash"]
        .as_str()
        .expect("response canonical hash")
        .to_owned();
    document["canonical_hash"] = Value::String(String::new());
    assert_eq!(
        canonical_hash(&document).expect("replay response canonical hash"),
        reported_hash,
        "canonical hash replay failed for {schema_relative_path}"
    );
    document["canonical_hash"] = Value::String(reported_hash);
    document
}

fn expect_error_code<T>(result: migration_stream::Result<T>, expected_code: &str) -> KernelError {
    match result {
        Ok(_) => panic!("read request should be rejected with {expected_code}"),
        Err(error) => {
            assert_eq!(error.code(), expected_code, "unexpected error: {error}");
            error
        }
    }
}

fn tamper_cursor(cursor: &str) -> String {
    let mut bytes = cursor.as_bytes().to_vec();
    let last = bytes.last_mut().expect("nonempty cursor");
    *last = if *last == b'a' { b'b' } else { b'a' };
    String::from_utf8(bytes).expect("cursor remains ASCII")
}

fn contains_key(value: &Value, sought: &str) -> bool {
    match value {
        Value::Object(object) => {
            object.contains_key(sought) || object.values().any(|value| contains_key(value, sought))
        }
        Value::Array(values) => values.iter().any(|value| contains_key(value, sought)),
        _ => false,
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn all_practitioner_read_contracts_are_bounded_scoped_and_schema_valid() {
    let fixture = prepare_governed_fixture();
    let reads = ReadService::new(&fixture.workspace);
    let default_page = PageRequest::default();
    let one_item_page = PageRequest {
        page_size: 1,
        cursor: None,
    };

    let job = reads
        .job_summary(&fixture.job_id)
        .expect("read accepted job summary");
    assert_eq!(job.data.counts.accepted_row_count, 4);
    assert_eq!(job.data.counts.accepted_file_count, 1);
    assert_eq!(job.data.counts.profiling_run_count, 1);
    let governance = job
        .data
        .selected_governance
        .as_ref()
        .expect("selected governed validation");
    assert_eq!(governance.validation_run_id, fixture.validation_run_id);
    let job_json = assert_contract(&job, "schemas/read/job-summary.schema.json");

    let files = reads
        .file_manifest(&fixture.job_id, &one_item_page)
        .expect("read accepted file manifest");
    assert_eq!(files.items.len(), 1);
    assert_eq!(files.items[0].path, SOURCE_FILE);
    assert_eq!(files.items[0].row_count, 4);
    assert_eq!(files.items[0].profiling_state, "COMPLETE");
    let files_json = assert_contract(&files, "schemas/read/file-manifest-page.schema.json");

    let profiling_first = reads
        .profiling_summary(&fixture.profiling_run_id, false, &one_item_page)
        .expect("read first redacted profiling page");
    assert_eq!(profiling_first.items.len(), 1);
    assert!(profiling_first.items[0].samples.is_none());
    let profiling_cursor = profiling_first
        .page
        .next_cursor
        .clone()
        .expect("profiling has another column");
    let profiling_second = reads
        .profiling_summary(
            &fixture.profiling_run_id,
            false,
            &PageRequest {
                page_size: 1,
                cursor: Some(profiling_cursor.clone()),
            },
        )
        .expect("read next redacted profiling page");
    assert_ne!(profiling_first.items[0], profiling_second.items[0]);
    let profiling_json = assert_contract(
        &profiling_first,
        "schemas/read/profiling-summary-page.schema.json",
    );
    let profiling_second_json = assert_contract(
        &profiling_second,
        "schemas/read/profiling-summary-page.schema.json",
    );
    assert!(
        reads
            .profiling_summary(
                &fixture.profiling_run_id,
                true,
                &PageRequest {
                    page_size: 1,
                    cursor: Some(profiling_cursor),
                },
            )
            .is_err(),
        "a cursor scoped to redacted profiling must not authorize sample disclosure"
    );

    let profile = reads
        .profile_view(PROFILE_ID, PROFILE_VERSION, &fixture.profile_hash)
        .expect("read exact immutable approved profile");
    assert_eq!(profile.data.lifecycle_state, "APPROVED");
    assert_eq!(profile.data.approvals.len(), 1);
    let profile_json = assert_contract(&profile, "schemas/read/profile-view.schema.json");

    let mapping_first = reads
        .mapping_coverage(
            MAPPING_ID,
            MAPPING_VERSION,
            &fixture.mapping_hash,
            &one_item_page,
        )
        .expect("read first mapping coverage page");
    assert_eq!(mapping_first.items.len(), 1);
    let mapping_cursor = mapping_first
        .page
        .next_cursor
        .clone()
        .expect("mapping coverage has another item");
    let mapping_second = reads
        .mapping_coverage(
            MAPPING_ID,
            MAPPING_VERSION,
            &fixture.mapping_hash,
            &PageRequest {
                page_size: 1,
                cursor: Some(mapping_cursor),
            },
        )
        .expect("read next mapping coverage page");
    assert_ne!(mapping_first.items[0], mapping_second.items[0]);
    let mapping_json = assert_contract(
        &mapping_first,
        "schemas/read/mapping-coverage-page.schema.json",
    );
    let mapping_second_json = assert_contract(
        &mapping_second,
        "schemas/read/mapping-coverage-page.schema.json",
    );
    let mapping_all = reads
        .mapping_coverage(
            MAPPING_ID,
            MAPPING_VERSION,
            &fixture.mapping_hash,
            &default_page,
        )
        .expect("read complete bounded mapping coverage");
    assert_eq!(mapping_all.items.len(), 5);
    let source_binding = mapping_all
        .items
        .iter()
        .find(|item| item.binding_id.as_deref() == Some("synthetic.customer.source-id"))
        .expect("direct source binding");
    assert_eq!(source_binding.kind, "MAPPED");
    assert_eq!(source_binding.mapping_kind.as_deref(), Some("direct"));
    assert_eq!(source_binding.source_file.as_deref(), Some(SOURCE_FILE));
    assert_eq!(
        source_binding.source_column_name.as_deref(),
        Some("migrationPartnerId")
    );
    assert_eq!(source_binding.source_column_ordinal, Some(0));
    assert_eq!(source_binding.entity_id.as_deref(), Some("Customer"));
    assert_eq!(source_binding.field_id.as_deref(), Some("sourceCustomerId"));
    let constant_binding = mapping_all
        .items
        .iter()
        .find(|item| item.binding_id.as_deref() == Some("synthetic.customer.duplicate-group"))
        .expect("constant default binding");
    assert_eq!(constant_binding.kind, "DEFAULTED");
    assert_eq!(constant_binding.mapping_kind.as_deref(), Some("constant"));
    assert!(constant_binding.source_file.is_none());
    assert!(constant_binding.source_column_name.is_none());
    assert!(constant_binding.source_column_ordinal.is_none());
    assert_eq!(constant_binding.entity_id.as_deref(), Some("Customer"));
    assert_eq!(
        constant_binding.field_id.as_deref(),
        Some("duplicateGroupKey")
    );

    let validation = reads
        .validation_summary(&fixture.validation_run_id)
        .expect("read validation summary");
    assert_eq!(validation.data.state, "COMPLETE");
    assert_eq!(validation.data.rows_processed, 4);
    assert_eq!(validation.data.issue_count, 5);
    let validation_json =
        assert_contract(&validation, "schemas/read/validation-summary.schema.json");

    let issue_filters = IssueFilters::default();
    let issues_first = reads
        .issue_list(
            &fixture.validation_run_id,
            &issue_filters,
            &PageRequest {
                page_size: 2,
                cursor: None,
            },
        )
        .expect("read first issue page");
    assert_eq!(issues_first.items.len(), 2);
    let issue_cursor = issues_first
        .page
        .next_cursor
        .clone()
        .expect("five issues require another page");
    let issues_second = reads
        .issue_list(
            &fixture.validation_run_id,
            &issue_filters,
            &PageRequest {
                page_size: 2,
                cursor: Some(issue_cursor.clone()),
            },
        )
        .expect("read next issue page");
    assert_eq!(issues_second.items.len(), 2);
    assert!(
        issues_first.items.last().expect("first issue").issue_id
            < issues_second.items.first().expect("next issue").issue_id
    );
    let issues_json = assert_contract(&issues_first, "schemas/read/issue-list-page.schema.json");
    let issues_second_json =
        assert_contract(&issues_second, "schemas/read/issue-list-page.schema.json");
    let issues_third = reads
        .issue_list(
            &fixture.validation_run_id,
            &issue_filters,
            &PageRequest {
                page_size: 2,
                cursor: issues_second.page.next_cursor.clone(),
            },
        )
        .expect("read final issue page");
    assert_eq!(issues_third.items.len(), 1);
    assert!(issues_third.page.next_cursor.is_none());
    let issues_third_json =
        assert_contract(&issues_third, "schemas/read/issue-list-page.schema.json");

    assert!(
        reads
            .issue_list(
                &fixture.validation_run_id,
                &IssueFilters {
                    severity: Some(IssueSeverity::Warning),
                    ..IssueFilters::default()
                },
                &PageRequest {
                    page_size: 2,
                    cursor: Some(issue_cursor.clone()),
                },
            )
            .is_err(),
        "an issue cursor cannot cross filter scope"
    );
    assert!(
        reads
            .issue_list(
                &fixture.validation_run_id,
                &issue_filters,
                &PageRequest {
                    page_size: 2,
                    cursor: Some(tamper_cursor(&issue_cursor)),
                },
            )
            .is_err(),
        "a checksum-tampered cursor must be rejected"
    );

    let row = reads
        .row_detail(&fixture.validation_run_id, SOURCE_FILE, 1)
        .expect("read exact accepted row detail");
    assert_eq!(row.data.source_values.len(), 4);
    assert!(
        row.data.source_values.iter().any(|field| {
            field.column_name == "notes" && field.value == "first line\nsecond line"
        })
    );
    let row_json = assert_contract(&row, "schemas/read/row-detail.schema.json");

    let reconciliation = reads
        .reconciliation_summary(
            &fixture.job_id,
            Some(&fixture.validation_run_id),
            &default_page,
        )
        .expect("read governed reconciliation summary");
    assert!(reconciliation.aggregate.closed);
    assert_eq!(reconciliation.aggregate.validation_rows_processed, Some(4));
    assert_eq!(reconciliation.aggregate.validation_issue_count, Some(5));
    let reconciliation_json = assert_contract(
        &reconciliation,
        "schemas/read/reconciliation-summary.schema.json",
    );

    let cleanup = reads
        .operational_cleanup(&CleanupFilters::default(), &default_page)
        .expect("read empty bounded cleanup evidence");
    assert_eq!(
        cleanup.items,
        [] as [migration_stream::read_model::OperationalCleanupItem; 0]
    );
    let cleanup_json = assert_contract(
        &cleanup,
        "schemas/read/operational-cleanup-summary.schema.json",
    );

    expect_error_code(
        reads.file_manifest(
            &fixture.job_id,
            &PageRequest {
                page_size: MAXIMUM_PAGE_SIZE + 1,
                cursor: None,
            },
        ),
        "READ_MODEL_INVALID",
    );

    let broad_responses = [
        job_json,
        files_json,
        profiling_json,
        profiling_second_json,
        profile_json,
        mapping_json,
        mapping_second_json,
        validation_json,
        issues_json,
        issues_second_json,
        issues_third_json,
        reconciliation_json,
        cleanup_json,
    ];
    for response in &broad_responses {
        assert!(!contains_key(response, "source_values"));
        assert!(!contains_key(response, "canonical_projection"));
        assert!(
            !contains_key(response, "value"),
            "only exact row detail may expose source or projected values: {response}"
        );
        let serialized = serde_json::to_string(response).expect("serialize broad response");
        for source_value in [
            "Acme, S.A.",
            "first line\\nsecond line",
            "Αθήνα Α.Ε.",
            "東京株式会社",
            "leading zero 0007",
        ] {
            assert!(
                !serialized.contains(source_value),
                "broad response leaked synthetic source value {source_value:?}"
            );
        }
    }
    assert!(contains_key(&row_json, "source_values"));
    assert!(contains_key(&row_json, "canonical_projection"));
    assert!(contains_key(&row_json, "value"));
}

#[test]
fn inclusive_rule_identity_and_message_bounds_remain_read_schema_valid() {
    let fixture = prepare_governed_fixture_with_profile(|profile| {
        for (index, rule) in profile["rules"]
            .as_array_mut()
            .expect("synthetic profile rules")
            .iter_mut()
            .enumerate()
        {
            let rule_id = format!("{}{:02}", "R".repeat(126), index);
            assert_eq!(rule_id.chars().count(), 128);
            rule["ruleId"] = json!(rule_id);
            rule["message"] = json!("m".repeat(4096));
        }
    });
    let reads = ReadService::new(&fixture.workspace);
    let summary = reads
        .validation_summary(&fixture.validation_run_id)
        .expect("read validation summary at inclusive rule bounds");
    assert_contract(&summary, "schemas/read/validation-summary.schema.json");

    let issues = reads
        .issue_list(
            &fixture.validation_run_id,
            &IssueFilters::default(),
            &PageRequest {
                page_size: MAXIMUM_PAGE_SIZE,
                cursor: None,
            },
        )
        .expect("read issues at inclusive rule bounds");
    assert_ne!(
        issues.items,
        [] as [migration_stream::read_model::IssueListItem; 0]
    );
    for issue in &issues.items {
        assert_eq!(issue.rule_id.chars().count(), 128);
        assert_eq!(issue.display_message.chars().count(), 4096);
    }
    assert_contract(&issues, "schemas/read/issue-list-page.schema.json");

    let issue = issues.items.first().expect("at least one bounded issue");
    let row = reads
        .row_detail(
            &fixture.validation_run_id,
            &issue.source_file,
            issue.source_ordinal,
        )
        .expect("read row detail at inclusive rule bounds");
    assert_contract(&row, "schemas/read/row-detail.schema.json");
}

#[test]
fn practitioner_read_execution_deadline_fails_explicitly() {
    let fixture = prepare_governed_fixture();
    let reads = ReadService::with_query_timeout(&fixture.workspace, Duration::from_nanos(1))
        .expect("construct read service with a positive bounded deadline");
    expect_error_code(
        reads.job_summary(&fixture.job_id),
        "READ_QUERY_GUARD_EXCEEDED",
    );
}

#[test]
fn profile_read_replays_approval_and_lifecycle_hashes() {
    let fixture = prepare_governed_fixture();
    let database = fixture.workspace.join("state.sqlite3");
    let connection = Connection::open(&database).expect("open profile evidence for tamper proof");
    let (approver, reason): (String, String) = connection
        .query_row(
            "SELECT approver, reason FROM profile_approvals WHERE profile_hash = ?1",
            [&fixture.profile_hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("load original approval evidence");
    let approval_sequence: i64 = connection
        .query_row(
            "SELECT sequence FROM profile_lifecycle_events
             WHERE profile_hash = ?1 AND to_state = 'APPROVED'",
            [&fixture.profile_hash],
            |row| row.get(0),
        )
        .expect("load approval lifecycle event");
    let reads = ReadService::new(&fixture.workspace);

    with_immutability_guard_disabled(
        &connection,
        "profile_approvals_no_update",
        PROFILE_APPROVALS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE profile_approvals SET approver = ?1 WHERE profile_hash = ?2",
                    params![format!("{approver}-tampered"), fixture.profile_hash],
                )
                .expect("tamper approval actor");
        },
    );
    expect_error_code(
        reads.profile_view(PROFILE_ID, PROFILE_VERSION, &fixture.profile_hash),
        "INTEGRITY_FAILED",
    );
    with_immutability_guard_disabled(
        &connection,
        "profile_approvals_no_update",
        PROFILE_APPROVALS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE profile_approvals SET approver = ?1 WHERE profile_hash = ?2",
                    params![approver, fixture.profile_hash],
                )
                .expect("restore approval actor");
        },
    );

    with_immutability_guard_disabled(
        &connection,
        "profile_approvals_no_update",
        PROFILE_APPROVALS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE profile_approvals SET reason = ?1 WHERE profile_hash = ?2",
                    params![format!("{reason} [tampered]"), fixture.profile_hash],
                )
                .expect("tamper approval reason");
        },
    );
    expect_error_code(
        reads.profile_view(PROFILE_ID, PROFILE_VERSION, &fixture.profile_hash),
        "INTEGRITY_FAILED",
    );
    expect_error_code(
        reads.validation_summary(&fixture.validation_run_id),
        "INTEGRITY_FAILED",
    );
    with_immutability_guard_disabled(
        &connection,
        "profile_approvals_no_update",
        PROFILE_APPROVALS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE profile_approvals SET reason = ?1 WHERE profile_hash = ?2",
                    params![reason, fixture.profile_hash],
                )
                .expect("restore approval reason");
        },
    );

    with_immutability_guard_disabled(
        &connection,
        "profile_lifecycle_events_no_update",
        PROFILE_LIFECYCLE_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE profile_lifecycle_events SET to_state = 'DEPRECATED' WHERE sequence = ?1",
                    [approval_sequence],
                )
                .expect("tamper lifecycle state");
        },
    );
    expect_error_code(
        reads.profile_view(PROFILE_ID, PROFILE_VERSION, &fixture.profile_hash),
        "INTEGRITY_FAILED",
    );
}

#[test]
fn mapping_coverage_rejects_rehashed_rows_not_bound_to_the_valid_attempt() {
    let fixture = prepare_governed_fixture();
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open mapping evidence");
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("permit deliberate integrity tamper");
    let (document_json, coverage_json): (String, String) = connection
        .query_row(
            "SELECT document_json, coverage_json FROM mapping_versions
             WHERE mapping_id = ?1 AND mapping_version = ?2",
            params![MAPPING_ID, MAPPING_VERSION],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("load mapping evidence for deliberate tamper");
    let mut document: Value = serde_json::from_str(&document_json).expect("parse mapping document");
    document["author"] = json!("rehashed practitioner-read tamper");
    let mapping_hash = canonical_hash(&document).expect("hash changed mapping document");
    let mut coverage: Value = serde_json::from_str(&coverage_json).expect("parse coverage");
    coverage["mappingHash"] = json!(mapping_hash);
    let coverage_hash = canonical_hash(&coverage).expect("hash changed mapping coverage");
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
                        mapping_hash,
                        canonical_json_string(&document).expect("canonical mapping document"),
                        canonical_json_string(&coverage).expect("canonical mapping coverage"),
                        coverage_hash,
                        MAPPING_ID,
                        MAPPING_VERSION
                    ],
                )
                .expect("rehash mapping row without its immutable attempt");
        },
    );
    drop(connection);

    let error = expect_error_code(
        ReadService::new(&fixture.workspace).mapping_coverage(
            MAPPING_ID,
            MAPPING_VERSION,
            &mapping_hash,
            &PageRequest::default(),
        ),
        "INTEGRITY_FAILED",
    );
    assert!(
        error
            .to_string()
            .contains("immutable VALID registration attempt")
    );
}

#[test]
fn mapping_coverage_replays_exact_deterministic_coverage() {
    let fixture = prepare_governed_fixture();
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open mapping evidence");
    let coverage_json: String = connection
        .query_row(
            "SELECT coverage_json FROM mapping_versions
             WHERE mapping_id = ?1 AND mapping_version = ?2",
            params![MAPPING_ID, MAPPING_VERSION],
            |row| row.get(0),
        )
        .expect("read immutable mapping coverage");
    let mut coverage: Value = serde_json::from_str(&coverage_json).expect("parse mapping coverage");
    coverage["unmappedSourceColumns"] = json!([{
        "sourceFile": SOURCE_FILE,
        "sourceColumn": "migrationPartnerId",
        "sourceColumnOrdinal": 0
    }]);
    let coverage_json = canonical_json_string(&coverage).expect("canonical tampered coverage");
    let coverage_hash = canonical_hash(&coverage).expect("hash tampered coverage");
    with_immutability_guard_disabled(
        &connection,
        "mapping_versions_no_update",
        MAPPING_VERSIONS_UPDATE_GUARD_SQL,
        |connection| {
            connection
                .execute(
                    "UPDATE mapping_versions SET coverage_json = ?1, coverage_hash = ?2
                     WHERE mapping_id = ?3 AND mapping_version = ?4",
                    params![coverage_json, coverage_hash, MAPPING_ID, MAPPING_VERSION],
                )
                .expect("install internally rehashed coverage tamper");
        },
    );
    drop(connection);

    let error = expect_error_code(
        ReadService::new(&fixture.workspace).mapping_coverage(
            MAPPING_ID,
            MAPPING_VERSION,
            &fixture.mapping_hash,
            &PageRequest::default(),
        ),
        "INTEGRITY_FAILED",
    );
    assert!(
        error
            .to_string()
            .contains("coverage differs from deterministic profile/source replay")
    );
}

#[test]
fn practitioner_validation_reads_replay_result_issue_and_lifecycle_evidence() {
    let fixture = prepare_governed_fixture();
    let database = fixture.workspace.join("state.sqlite3");
    let connection = Connection::open(&database).expect("open validation evidence");
    let reads = ReadService::new(&fixture.workspace);

    let (issue_id, display_message): (String, String) = connection
        .query_row(
            "SELECT issue_id, display_message FROM validation_issues
             WHERE run_id = ?1 ORDER BY issue_id LIMIT 1",
            [&fixture.validation_run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("load one immutable issue");
    connection
        .execute(
            "UPDATE validation_issues SET display_message = display_message || ' [tampered]'
             WHERE run_id = ?1 AND issue_id = ?2",
            params![fixture.validation_run_id, issue_id],
        )
        .expect("tamper issue evidence");
    expect_error_code(
        reads.issue_list(
            &fixture.validation_run_id,
            &IssueFilters::default(),
            &PageRequest::default(),
        ),
        "INTEGRITY_FAILED",
    );
    connection
        .execute(
            "UPDATE validation_issues SET display_message = ?1
             WHERE run_id = ?2 AND issue_id = ?3",
            params![display_message, fixture.validation_run_id, issue_id],
        )
        .expect("restore issue evidence");

    let result_json: String = connection
        .query_row(
            "SELECT result_json FROM validation_runs WHERE id = ?1",
            [&fixture.validation_run_id],
            |row| row.get(0),
        )
        .expect("load immutable validation result");
    let mut result: Value = serde_json::from_str(&result_json).expect("parse validation result");
    result["rowsProcessed"] = json!(9_999);
    connection
        .execute(
            "UPDATE validation_runs SET result_json = ?1 WHERE id = ?2",
            params![
                canonical_json_string(&result).expect("canonical tampered result"),
                fixture.validation_run_id
            ],
        )
        .expect("tamper validation result bytes");
    expect_error_code(
        reads.validation_summary(&fixture.validation_run_id),
        "INTEGRITY_FAILED",
    );
    connection
        .execute(
            "UPDATE validation_runs SET result_json = ?1 WHERE id = ?2",
            params![result_json, fixture.validation_run_id],
        )
        .expect("restore validation result bytes");

    connection
        .execute(
            "UPDATE validation_events SET detail = 'tampered lifecycle evidence'
             WHERE run_id = ?1 AND state = 'COMPLETE'",
            [&fixture.validation_run_id],
        )
        .expect("tamper terminal validation event");
    expect_error_code(
        reads.row_detail(&fixture.validation_run_id, SOURCE_FILE, 1),
        "INTEGRITY_FAILED",
    );
}

#[test]
fn issue_pagination_rejects_a_running_membership_before_cursor_use() {
    let fixture = prepare_governed_fixture();
    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open validation evidence");
    connection
        .execute_batch("DROP TRIGGER validation_terminal_hash_no_update;")
        .expect("open a low-level test-only terminal checkpoint mutation window");
    connection
        .execute(
            "UPDATE validation_runs
             SET state = 'RUNNING', result_json = NULL, result_hash = NULL,
                 terminal_hash = NULL, completed_at = NULL
             WHERE id = ?1",
            [&fixture.validation_run_id],
        )
        .expect("simulate a still-changing validation membership");
    connection
        .execute_batch(
            "CREATE TRIGGER validation_terminal_hash_no_update
                 BEFORE UPDATE OF terminal_hash ON validation_runs
                 WHEN OLD.terminal_hash IS NOT NULL
                 BEGIN
                     SELECT RAISE(ABORT, 'validation terminal checkpoint is immutable');
                 END;",
        )
        .expect("restore the exact terminal checkpoint guard");

    let error = expect_error_code(
        ReadService::new(&fixture.workspace).issue_list(
            &fixture.validation_run_id,
            &IssueFilters::default(),
            &PageRequest::default(),
        ),
        "READ_MODEL_INVALID",
    );
    assert!(error.to_string().contains("membership is still changing"));
}

#[test]
fn row_detail_schema_matches_the_implemented_issue_cap() {
    let schema: Value = serde_json::from_slice(
        &fs::read(project_path("schemas/read/row-detail.schema.json"))
            .expect("read row-detail schema"),
    )
    .expect("parse row-detail schema");
    assert_eq!(
        schema.pointer("/$defs/detail/properties/issues/maxItems"),
        Some(&Value::from(200)),
    );
}

#[test]
fn file_manifest_cursor_cannot_invent_source_ordinals() {
    let temporary = tempfile::tempdir_in(project_path("target"))
        .expect("link-safe temporary directory beneath the build target");
    let archive = temporary.path().join("two-files.zip");
    write_two_file_package(&archive);
    let workspace = temporary.path().join("workspace");
    object_workspace_support::initialize_test_workspace(&workspace);
    let intake = ingest_package(&archive, &workspace, &intake_policy())
        .expect("ingest two-file synthetic package");
    let reads = ReadService::new(&workspace);
    let first = reads
        .file_manifest(
            &intake.job_id,
            &PageRequest {
                page_size: 1,
                cursor: None,
            },
        )
        .expect("read first file page");
    assert_eq!(first.items[0].path, "a.csv");
    assert_eq!(first.items[0].source_ordinal, 1);
    let second = reads
        .file_manifest(
            &intake.job_id,
            &PageRequest {
                page_size: 1,
                cursor: first.page.next_cursor,
            },
        )
        .expect("read authoritative second file page");
    assert_eq!(second.items[0].path, "b.csv");
    assert_eq!(second.items[0].source_ordinal, 2);

    let scope_hash = canonical_hash(&json!({
        "job_id": intake.job_id,
        "source_hash": intake.source_hash,
    }))
    .expect("hash file-manifest scope");
    let filter_hash = canonical_hash(&json!({})).expect("hash empty file-manifest filters");
    let forged = encode_cursor(
        "file-manifest/v1",
        &scope_hash,
        &filter_hash,
        json!(["a.csv", 99]),
    )
    .expect("construct structurally valid cursor with false ordinal");
    expect_error_code(
        reads.file_manifest(
            &intake.job_id,
            &PageRequest {
                page_size: 1,
                cursor: Some(forged),
            },
        ),
        "READ_MODEL_INVALID",
    );
}

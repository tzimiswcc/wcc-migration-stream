#![allow(clippy::default_trait_access, clippy::too_many_lines)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Barrier},
};

use axum::{
    body::{Body, to_bytes},
    http::{Method, Request as HttpRequest, StatusCode},
};
use jsonschema::PatternOptions;
use migration_stream::{
    KernelError,
    canonical::{canonical_hash, canonical_json_string, sha256_hex},
    decision_read_model::{DecisionAwareIssueDetail, DecisionQueueFilters, DecisionReadService},
    decision_service::{DecisionCommandResult, DecisionService},
    decisions::{
        CorrectionOperation, CorrectionOverlayRequest, DECISION_SCHEMA_VERSION,
        DecisionConcurrency, DecisionScope, IssueDecisionRequest, IssueDisposition,
    },
    intake::ingest_package,
    mapping::validate_mapping,
    policy::IntakePolicy,
    profile_registry::{RegistryLimits, approve_profile, register_profile},
    state::{integrity_check, recover_workspace},
    validation::{ValidationControl, ValidationPolicy, run_validation},
    workbench::{WorkbenchConfig, build_router},
};
use rusqlite::{Connection, OptionalExtension, params, types::ValueRef};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt as _;

const PROFILE_ID: &str = "synthetic.customer.readiness";
const PROFILE_VERSION: &str = "1.0.0";
const MAPPING_ID: &str = "synthetic.customer.valid";
const MAPPING_VERSION: &str = "1.0.0";

struct DecisionFixture {
    _temporary: TempDir,
    workspace: PathBuf,
    job_id: String,
    validation_run_id: String,
}

#[derive(Debug, Eq, PartialEq)]
struct InheritedEvidenceSnapshot {
    accepted_package: Vec<u8>,
    source_records: Vec<Vec<String>>,
    profile_versions: Vec<Vec<String>>,
    profile_approvals: Vec<Vec<String>>,
    mapping_versions: Vec<Vec<String>>,
    validation_runs: Vec<Vec<String>>,
    validation_issues: Vec<Vec<String>>,
}

struct ReevaluationIssueTamperTarget {
    reevaluation_id: String,
    issue_id: String,
    source_validation_run_id: String,
    source_issue_id: String,
    severity: String,
    rule_id: String,
    issue_code: String,
    display_message: String,
    entity_id: String,
    field_id: String,
    source_file: String,
    source_ordinal: i64,
    line_start: i64,
    line_end: i64,
    lineage_json: String,
    engine_issue_hash: String,
}

struct CompleteReevaluationTamperContext {
    identity_hash: String,
    dependency_scope_hash: String,
    source_issue_id: String,
    source_validation_run_id: String,
    rows_processed: i64,
    issue_count: i64,
    created_at: String,
    completed_at: String,
}

struct BoundExceededEvidence {
    state: String,
    result_json: Option<String>,
    result_hash: Option<String>,
    terminal_hash: Option<String>,
    issue_count: i64,
    rows_processed: i64,
    error_code: String,
    error_message: String,
    attempt_id: String,
    overlay_id: String,
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

fn prepare_fixture() -> DecisionFixture {
    prepare_fixture_with_options(None, false, None)
}

fn prepare_fixture_with_name_maximum(name_maximum: Option<u64>) -> DecisionFixture {
    prepare_fixture_with_options(name_maximum, false, None)
}

fn prepare_fixture_with_same_field_issues() -> DecisionFixture {
    prepare_fixture_with_options(None, true, None)
}

fn prepare_fixture_with_maximum_unique_keys(maximum_unique_keys: usize) -> DecisionFixture {
    prepare_fixture_with_options(None, false, Some(maximum_unique_keys))
}

fn prepare_fixture_with_options(
    name_maximum: Option<u64>,
    add_same_field_rule: bool,
    maximum_unique_keys: Option<usize>,
) -> DecisionFixture {
    let temporary = tempfile::tempdir_in(project_path("target"))
        .expect("link-safe temporary directory beneath target");
    let workspace = temporary.path().join("private-decision-workspace");
    fs::create_dir(&workspace).expect("create selected object workspace");
    migration_stream::object_workspace::establish_for_local_automation(
        &workspace,
        "Synthetic decision workspace",
        "Customer governed decisions",
        "Automated decision proof",
    )
    .expect("initialize object workspace");
    let intake = ingest_package(
        &project_path("fixtures/packages/valid.zip"),
        &workspace,
        &intake_policy(),
    )
    .expect("ingest bounded synthetic package");
    let profile_path = if name_maximum.is_some() || add_same_field_rule {
        let mut profile: Value = serde_json::from_slice(
            &fs::read(project_path(
                "fixtures/wp003/synthetic-profile.reviewed.json",
            ))
            .expect("read reviewed synthetic profile"),
        )
        .expect("parse reviewed synthetic profile");
        if let Some(maximum) = name_maximum {
            let fields = profile["entities"][0]["fields"]
                .as_array_mut()
                .expect("synthetic profile fields");
            fields
                .iter_mut()
                .find(|field| field["fieldId"] == "name")
                .expect("synthetic name field")["maxLength"] = json!(maximum);
            let rules = profile["rules"]
                .as_array_mut()
                .expect("synthetic profile rules");
            rules
                .iter_mut()
                .find(|rule| rule["ruleId"] == "SYN-LEN-001")
                .expect("synthetic maximum-length rule")["parameters"]["maximum"] = json!(maximum);
        }
        if add_same_field_rule {
            profile["rules"]
                .as_array_mut()
                .expect("synthetic profile rules")
                .push(json!({
                    "ruleId": "SYN-NAME-SECOND-001",
                    "severity": "error",
                    "kind": "allowed_values",
                    "target": "Customer.name",
                    "parameters": { "values": ["SYNTHETIC-ALLOWLIST"] },
                    "message": "Synthetic same-field chain must use the exact allow-listed value."
                }));
        }
        let path = temporary.path().join("custom-reviewed-profile.json");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&profile).expect("serialize large-text profile"),
        )
        .expect("write large-text synthetic profile");
        path
    } else {
        project_path("fixtures/wp003/synthetic-profile.reviewed.json")
    };
    let registered = register_profile(
        &workspace,
        &project_path("schemas/migration-object-profile.schema.json"),
        &profile_path,
        &RegistryLimits::default(),
    )
    .expect("register reviewed synthetic profile");
    let approved = approve_profile(
        &workspace,
        PROFILE_ID,
        PROFILE_VERSION,
        &registered.profile_hash,
        "WP-005 governed decision integration test",
        "Authorize only this synthetic profile for local mapping and validation proof",
        true,
        &RegistryLimits::default(),
    )
    .expect("approve exact synthetic profile");
    let mapping_path = temporary.path().join("mapping.json");
    let template = fs::read_to_string(project_path("fixtures/wp003/valid-mapping.template.json"))
        .expect("read synthetic mapping template");
    fs::write(
        &mapping_path,
        template.replace("__PROFILE_SHA256__", &approved.profile_hash),
    )
    .expect("write finalized synthetic mapping");
    validate_mapping(
        &workspace,
        &intake.job_id,
        &project_path("schemas/source-mapping-version.schema.json"),
        &project_path("schemas/migration-object-profile.schema.json"),
        &mapping_path,
    )
    .expect("validate exact synthetic mapping");
    let mut policy =
        ValidationPolicy::from_path(&project_path("fixtures/wp003/validation-policy.json"))
            .expect("load bounded validation policy");
    if let Some(maximum_unique_keys) = maximum_unique_keys {
        policy.maximum_unique_keys = maximum_unique_keys;
    }
    let validation = run_validation(
        &workspace,
        &intake.job_id,
        MAPPING_ID,
        MAPPING_VERSION,
        &project_path("schemas/migration-object-profile.schema.json"),
        &policy,
    )
    .expect("create immutable source validation evidence");
    assert_eq!(
        validation.result.issue_count,
        if add_same_field_rule { 8 } else { 5 }
    );
    DecisionFixture {
        _temporary: temporary,
        workspace,
        job_id: intake.job_id,
        validation_run_id: validation.run_id,
    }
}

fn open_database(fixture: &DecisionFixture) -> Connection {
    Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open authoritative test state")
}

fn decision_service(fixture: &DecisionFixture) -> DecisionService {
    DecisionService::new(
        &fixture.workspace,
        project_path("schemas/migration-object-profile.schema.json"),
    )
}

fn read_service(fixture: &DecisionFixture) -> DecisionReadService {
    DecisionReadService::new(&fixture.workspace)
}

fn issue_scope(fixture: &DecisionFixture, rule_id: &str, ordinal: u64) -> DecisionScope {
    type ScopeRow = (
        String,
        String,
        String,
        i64,
        i64,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    );
    let row: ScopeRow = open_database(fixture)
        .query_row(
            "SELECT vi.issue_id, vi.issue_hash, vi.source_file,
                    vi.line_start, vi.line_end, vi.entity_id, vi.field_id,
                    vr.profile_id, vr.profile_version, vr.profile_hash,
                    vr.mapping_id, vr.mapping_version, vr.mapping_hash, vr.job_id
             FROM validation_issues vi
             JOIN validation_runs vr ON vr.id = vi.run_id
             WHERE vi.run_id = ?1 AND vi.rule_id = ?2 AND vi.source_ordinal = ?3",
            params![
                fixture.validation_run_id,
                rule_id,
                i64::try_from(ordinal).expect("synthetic ordinal fits SQLite")
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                ))
            },
        )
        .expect("select exact synthetic validation issue");
    assert_eq!(row.13, fixture.job_id);
    DecisionScope {
        job_id: fixture.job_id.clone(),
        validation_run_id: fixture.validation_run_id.clone(),
        issue_id: row.0,
        issue_hash: row.1,
        accepted_source_file: row.2,
        source_ordinal: ordinal,
        line_start: u64::try_from(row.3).expect("positive synthetic line start"),
        line_end: u64::try_from(row.4).expect("positive synthetic line end"),
        canonical_entity_id: row.5,
        canonical_field_id: row.6,
        profile_id: row.7,
        profile_version: row.8,
        profile_hash: row.9,
        mapping_id: row.10,
        mapping_version: row.11,
        mapping_hash: row.12,
    }
}

fn issue_detail(fixture: &DecisionFixture, scope: &DecisionScope) -> DecisionAwareIssueDetail {
    read_service(fixture)
        .issue_detail(&fixture.job_id, &fixture.validation_run_id, &scope.issue_id)
        .expect("read exact decision-aware issue detail")
}

fn request(
    fixture: &DecisionFixture,
    scope: DecisionScope,
    idempotency_key: &str,
    disposition: IssueDisposition,
    correction_overlay: Option<CorrectionOverlayRequest>,
) -> IssueDecisionRequest {
    let detail = issue_detail(fixture, &scope);
    let uses_effective_hash = matches!(
        disposition,
        IssueDisposition::Corrected | IssueDisposition::AcceptedAsIs
    );
    IssueDecisionRequest {
        schema_version: DECISION_SCHEMA_VERSION,
        idempotency_key: idempotency_key.to_owned(),
        disposition,
        scope,
        concurrency: DecisionConcurrency {
            expected_active_decision_id: detail.data.concurrency.expected_active_decision_id,
            expected_active_decision_generation: detail.data.concurrency.expected_active_generation,
            expected_effective_value_hash: uses_effective_hash
                .then_some(detail.data.concurrency.expected_effective_value_hash),
        },
        practitioner_id: "synthetic-practitioner-005".to_owned(),
        reason: format!(
            "Bounded synthetic {} evidence without source or result authority",
            disposition.as_str()
        ),
        evidence_reference: Some("WP005-SYNTHETIC-EVIDENCE-ONLY".to_owned()),
        authority_separation_acknowledged: true,
        row_scope_exclusion_acknowledged: disposition == IssueDisposition::Excluded,
        correction_overlay,
    }
}

fn replacement(value: &str) -> CorrectionOverlayRequest {
    CorrectionOverlayRequest {
        operation: CorrectionOperation::ReplaceEffectiveText,
        replacement_text: Some(value.to_owned()),
    }
}

fn expect_error<T>(result: migration_stream::Result<T>, code: &str) -> KernelError {
    match result {
        Ok(_) => panic!("expected {code} failure"),
        Err(error) => {
            assert_eq!(error.code(), code, "unexpected error: {error}");
            error
        }
    }
}

fn expect_tombstone_read_blocked<T>(result: migration_stream::Result<T>, surface: &str) {
    let error = expect_error(result, "DECISION_INTEGRITY_FAILED");
    assert!(
        error.to_string().contains("quarantine"),
        "{surface} returned an unexpected tombstone diagnostic: {error}"
    );
}

fn table_count(connection: &Connection, table: &str) -> i64 {
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count bounded evidence table")
}

fn snapshot_query(connection: &Connection, sql: &str) -> Vec<Vec<String>> {
    let mut statement = connection.prepare(sql).expect("prepare snapshot query");
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns)
                .map(|index| match row.get_ref(index)? {
                    ValueRef::Null => Ok("NULL".to_owned()),
                    ValueRef::Integer(value) => Ok(format!("I:{value}")),
                    ValueRef::Real(value) => Ok(format!("R:{value:?}")),
                    ValueRef::Text(value) => Ok(format!("T:{}", String::from_utf8_lossy(value))),
                    ValueRef::Blob(value) => Ok(format!("B:{value:?}")),
                })
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .expect("execute snapshot query")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect immutable snapshot rows")
}

fn inherited_snapshot(fixture: &DecisionFixture) -> InheritedEvidenceSnapshot {
    let connection = open_database(fixture);
    let source_hash: String = connection
        .query_row(
            "SELECT source_hash FROM jobs WHERE id = ?1",
            [&fixture.job_id],
            |row| row.get(0),
        )
        .expect("accepted package hash");
    InheritedEvidenceSnapshot {
        accepted_package: fs::read(
            fixture
                .workspace
                .join("packages")
                .join(format!("{source_hash}.zip")),
        )
        .expect("read accepted immutable package"),
        source_records: snapshot_query(
            &connection,
            "SELECT * FROM source_records ORDER BY record_id",
        ),
        profile_versions: snapshot_query(
            &connection,
            "SELECT * FROM profile_versions ORDER BY profile_id, profile_version, profile_hash",
        ),
        profile_approvals: snapshot_query(
            &connection,
            "SELECT * FROM profile_approvals ORDER BY profile_id, profile_version, profile_hash",
        ),
        mapping_versions: snapshot_query(
            &connection,
            "SELECT * FROM mapping_versions ORDER BY mapping_id, mapping_version",
        ),
        validation_runs: snapshot_query(&connection, "SELECT * FROM validation_runs ORDER BY id"),
        validation_issues: snapshot_query(
            &connection,
            "SELECT * FROM validation_issues ORDER BY run_id, issue_id",
        ),
    }
}

fn assert_effective(result: &DecisionCommandResult, disposition: &str) {
    assert_eq!(
        result.schema_version,
        "migration-stream/write/decision-command-result/v1"
    );
    assert_eq!(result.data.disposition, disposition);
    assert_eq!(result.data.lifecycle_outcome, "EFFECTIVE");
    assert!(result.data.active);
    assert_eq!(result.data.integrity_status, "VERIFIED");
    assert_eq!(result.canonical_hash.len(), 64);
}

fn assert_read_contract(value: &Value, schema_path: &str) {
    let schema: Value = serde_json::from_slice(
        &fs::read(project_path(schema_path)).expect("read versioned read contract"),
    )
    .expect("parse versioned read contract");
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .with_pattern_options(PatternOptions::fancy_regex())
        .build(&schema)
        .expect("compile versioned read contract");
    assert!(
        validator.is_valid(value),
        "response must satisfy {schema_path}: {value}"
    );
}

fn assert_wp005_guards(connection: &Connection) {
    let guarded = [
        ("decision_attempts", "reason"),
        ("decision_attempt_events", "state"),
        ("correction_overlays", "reason"),
        ("reevaluation_runs", "state"),
        ("reevaluation_events", "state"),
        ("reevaluation_issues", "outcome"),
        ("issue_decisions", "reason"),
        ("correction_effectivations", "effective_value_hash"),
    ];
    for (table, column) in guarded {
        let update = format!(
            "UPDATE {table} SET {column} = {column} WHERE rowid = (SELECT rowid FROM {table} LIMIT 1)"
        );
        let update_error = connection
            .execute(&update, [])
            .expect_err("WP-005 immutable row update must be rejected");
        assert!(
            update_error.to_string().contains("immutable"),
            "unexpected {table} update result: {update_error}"
        );
        let delete =
            format!("DELETE FROM {table} WHERE rowid = (SELECT rowid FROM {table} LIMIT 1)");
        let delete_error = connection
            .execute(&delete, [])
            .expect_err("WP-005 immutable row deletion must be rejected");
        assert!(
            delete_error.to_string().contains("immutable"),
            "unexpected {table} delete result: {delete_error}"
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn all_dispositions_append_attributed_history_and_preserve_inherited_evidence() {
    let fixture = prepare_fixture();
    let inherited_before = inherited_snapshot(&fixture);
    let service = decision_service(&fixture);

    let corrected_scope = issue_scope(&fixture, "SYN-PATTERN-001", 2);
    let corrected = service
        .apply(&request(
            &fixture,
            corrected_scope.clone(),
            "disposition-corrected-1",
            IssueDisposition::Corrected,
            Some(replacement("Athens Synthetic Name")),
        ))
        .expect("commit bounded correction and qualifying re-evaluation");
    assert_effective(&corrected, "CORRECTED");
    assert_eq!(corrected.data.reevaluation_state, "COMPLETE");
    assert!(!corrected.data.original_issue_present);
    assert!(corrected.data.correction_overlay_id.is_some());

    let waived = service
        .apply(&request(
            &fixture,
            issue_scope(&fixture, "SYN-PATTERN-001", 3),
            "disposition-waived-1",
            IssueDisposition::Waived,
            None,
        ))
        .expect("append bounded waiver evidence");
    assert_effective(&waived, "WAIVED");
    assert_eq!(waived.data.reevaluation_state, "NOT_REQUIRED");

    let accepted = service
        .apply(&request(
            &fixture,
            issue_scope(&fixture, "SYN-UNIQUE-001", 3),
            "disposition-accepted-1",
            IssueDisposition::AcceptedAsIs,
            None,
        ))
        .expect("append accepted-as-is evidence");
    assert_effective(&accepted, "ACCEPTED_AS_IS");

    let excluded = service
        .apply(&request(
            &fixture,
            issue_scope(&fixture, "SYN-UNIQUE-001", 4),
            "disposition-excluded-1",
            IssueDisposition::Excluded,
            None,
        ))
        .expect("append exact row-exclusion evidence");
    assert_effective(&excluded, "EXCLUDED");

    let connection = open_database(&fixture);
    assert_eq!(table_count(&connection, "decision_attempts"), 4);
    assert_eq!(table_count(&connection, "issue_decisions"), 4);
    assert_eq!(table_count(&connection, "correction_overlays"), 1);
    assert_eq!(table_count(&connection, "reevaluation_runs"), 1);
    assert_eq!(table_count(&connection, "correction_effectivations"), 1);
    let attributed: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM issue_decisions
             WHERE practitioner_id = 'synthetic-practitioner-005'
               AND reason LIKE 'Bounded synthetic % evidence without source or result authority'",
            [],
            |row| row.get(0),
        )
        .expect("count exact practitioner attribution");
    assert_eq!(attributed, 4);

    let history = read_service(&fixture)
        .decision_history(
            &fixture.job_id,
            &fixture.validation_run_id,
            &corrected_scope.issue_id,
            &Default::default(),
        )
        .expect("read immutable correction history");
    assert_eq!(history.items.len(), 1);
    assert_eq!(history.items[0].disposition, "CORRECTED");
    assert!(history.items[0].active);

    let overlay = read_service(&fixture)
        .correction_detail(
            corrected
                .data
                .correction_overlay_id
                .as_deref()
                .expect("effective overlay ID"),
        )
        .expect("read exact correction overlay detail");
    assert_eq!(
        overlay.data.overlay_value.as_deref(),
        Some("Athens Synthetic Name")
    );
    assert_eq!(
        overlay.data.original_canonical_value.as_deref(),
        Some("Αθήνα Α.Ε.")
    );
    assert!(overlay.data.active);

    assert_wp005_guards(&connection);
    drop(connection);
    assert_eq!(inherited_snapshot(&fixture), inherited_before);
    let integrity = integrity_check(&fixture.workspace).expect("replay all immutable evidence");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
fn non_correction_supersession_preserves_the_effective_field_overlay() {
    for (index, disposition) in [
        IssueDisposition::Waived,
        IssueDisposition::AcceptedAsIs,
        IssueDisposition::Excluded,
    ]
    .into_iter()
    .enumerate()
    {
        let fixture = prepare_fixture();
        let inherited_before = inherited_snapshot(&fixture);
        let service = decision_service(&fixture);
        let scope = issue_scope(&fixture, "SYN-PATTERN-001", 2);
        let corrected_value = format!("SYNTHETIC-CORRECTED-{index}");
        let corrected = service
            .apply(&request(
                &fixture,
                scope.clone(),
                &format!("non-correction-preservation-corrected-{index}"),
                IssueDisposition::Corrected,
                Some(replacement(&corrected_value)),
            ))
            .expect("commit the field-level correction before supersession");
        let overlay_id = corrected
            .data
            .correction_overlay_id
            .clone()
            .expect("effective correction overlay ID");
        let before = issue_detail(&fixture, &scope);
        assert_eq!(
            before.data.field_context.active_overlay_id.as_deref(),
            Some(overlay_id.as_str())
        );
        assert_eq!(
            before
                .data
                .field_context
                .effective_canonical_value
                .as_deref(),
            Some(corrected_value.as_str())
        );
        let effective_hash = before.data.field_context.effective_value_hash.clone();

        let superseding = service
            .apply(&request(
                &fixture,
                scope.clone(),
                &format!("non-correction-preservation-head-{index}"),
                disposition,
                None,
            ))
            .expect("append non-correction issue-decision head");
        assert_effective(&superseding, disposition.as_str());
        let after = issue_detail(&fixture, &scope);
        let active_head = after
            .data
            .active_decision
            .as_ref()
            .expect("superseding decision head");
        assert_eq!(active_head.disposition, disposition.as_str());
        assert_eq!(active_head.generation, 2);
        assert_eq!(
            after.data.field_context.active_overlay_id.as_deref(),
            Some(overlay_id.as_str()),
            "non-correction decision incorrectly deactivated field authority"
        );
        assert_eq!(
            after
                .data
                .field_context
                .effective_canonical_value
                .as_deref(),
            Some(corrected_value.as_str())
        );
        assert_eq!(
            after.data.field_context.effective_value_hash, effective_hash,
            "WAIVED/ACCEPTED_AS_IS/EXCLUDED must not change effective identity"
        );
        let overlay = read_service(&fixture)
            .correction_detail(&overlay_id)
            .expect("read retained field-level overlay");
        assert!(overlay.data.active);
        assert!(overlay.data.superseded_by_overlay_id.is_none());

        let history = read_service(&fixture)
            .decision_history(
                &fixture.job_id,
                &fixture.validation_run_id,
                &scope.issue_id,
                &Default::default(),
            )
            .expect("read superseded issue-decision history");
        assert_eq!(history.items.len(), 2);
        assert_eq!(history.items[0].disposition, "CORRECTED");
        assert!(!history.items[0].active);
        assert_eq!(history.items[1].disposition, disposition.as_str());
        assert!(history.items[1].active);
        assert_eq!(
            table_count(&open_database(&fixture), "correction_effectivations"),
            1
        );
        assert_eq!(inherited_snapshot(&fixture), inherited_before);
        let integrity = integrity_check(&fixture.workspace)
            .expect("replay retained overlay under non-correction head");
        assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
    }
}

#[test]
fn same_row_field_corrections_from_different_issues_form_one_effective_hash_chain() {
    let fixture = prepare_fixture_with_same_field_issues();
    let inherited_before = inherited_snapshot(&fixture);
    let service = decision_service(&fixture);
    let pattern_scope = issue_scope(&fixture, "SYN-PATTERN-001", 2);
    let allowed_scope = issue_scope(&fixture, "SYN-NAME-SECOND-001", 2);
    assert_ne!(pattern_scope.issue_id, allowed_scope.issue_id);
    assert_eq!(
        pattern_scope.accepted_source_file,
        allowed_scope.accepted_source_file
    );
    assert_eq!(pattern_scope.source_ordinal, allowed_scope.source_ordinal);
    assert_eq!(
        pattern_scope.canonical_entity_id,
        allowed_scope.canonical_entity_id
    );
    assert_eq!(
        pattern_scope.canonical_field_id,
        allowed_scope.canonical_field_id
    );

    let first = service
        .apply(&request(
            &fixture,
            pattern_scope.clone(),
            "cross-issue-field-correction-1",
            IssueDisposition::Corrected,
            Some(replacement("FIRST-FIELD-OVERLAY")),
        ))
        .expect("commit first issue-bound correction on shared field");
    assert!(!first.data.original_issue_present);
    let first_overlay_id = first
        .data
        .correction_overlay_id
        .clone()
        .expect("first cross-issue overlay ID");
    let first_effective_hash = issue_detail(&fixture, &allowed_scope)
        .data
        .field_context
        .effective_value_hash;

    let second_request = request(
        &fixture,
        allowed_scope.clone(),
        "cross-issue-field-correction-2",
        IssueDisposition::Corrected,
        Some(replacement("SYNTHETIC-ALLOWLIST")),
    );
    assert_eq!(
        second_request
            .concurrency
            .expected_effective_value_hash
            .as_deref(),
        Some(first_effective_hash.as_str())
    );
    assert_eq!(
        second_request
            .concurrency
            .expected_active_decision_generation,
        0,
        "issue heads are independent even though field authority is chained"
    );
    let second = service
        .apply(&second_request)
        .expect("commit second issue-bound correction over shared effective field");
    assert!(!second.data.original_issue_present);
    let second_overlay_id = second
        .data
        .correction_overlay_id
        .clone()
        .expect("second cross-issue overlay ID");

    for scope in [&pattern_scope, &allowed_scope] {
        let detail = issue_detail(&fixture, scope);
        assert_eq!(
            detail.data.field_context.active_overlay_id.as_deref(),
            Some(second_overlay_id.as_str())
        );
        assert_eq!(
            detail
                .data
                .field_context
                .effective_canonical_value
                .as_deref(),
            Some("SYNTHETIC-ALLOWLIST")
        );
        let head = detail.data.active_decision.expect("independent issue head");
        assert_eq!(head.disposition, "CORRECTED");
        assert_eq!(head.generation, 1);
    }

    let first_overlay = read_service(&fixture)
        .correction_detail(&first_overlay_id)
        .expect("read first cross-issue overlay");
    assert!(!first_overlay.data.active);
    assert_eq!(
        first_overlay.data.superseded_by_overlay_id.as_deref(),
        Some(second_overlay_id.as_str())
    );
    let second_overlay = read_service(&fixture)
        .correction_detail(&second_overlay_id)
        .expect("read second cross-issue overlay");
    assert!(second_overlay.data.active);
    assert_eq!(
        second_overlay.data.prior_effective_value.as_deref(),
        Some("FIRST-FIELD-OVERLAY")
    );

    let chain = {
        let connection = open_database(&fixture);
        let mut statement = connection
            .prepare(
                "SELECT ce.prior_effective_value_hash, ce.effective_value_hash
                 FROM correction_effectivations ce
                 JOIN decision_attempts a ON a.attempt_id = ce.decision_attempt_id
                 WHERE a.validation_run_id = ?1
                   AND a.accepted_source_file = ?2 AND a.source_ordinal = ?3
                   AND a.canonical_entity_id = ?4 AND a.canonical_field_id = ?5",
            )
            .expect("prepare cross-issue field-chain query");
        statement
            .query_map(
                params![
                    fixture.validation_run_id,
                    pattern_scope.accepted_source_file,
                    i64::try_from(pattern_scope.source_ordinal)
                        .expect("synthetic ordinal fits SQLite"),
                    pattern_scope.canonical_entity_id,
                    pattern_scope.canonical_field_id,
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .expect("query cross-issue field chain")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect cross-issue field chain")
    };
    assert_eq!(chain.len(), 2);
    assert!(
        chain
            .iter()
            .any(|(_, effective)| effective == &first_effective_hash)
    );
    assert!(
        chain
            .iter()
            .any(|(prior, _)| prior == &first_effective_hash),
        "the second issue-bound correction did not consume the first field tip"
    );
    assert_eq!(inherited_snapshot(&fixture), inherited_before);
    let integrity = integrity_check(&fixture.workspace).expect("replay cross-issue field chain");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
fn competing_writers_are_serialized_and_never_last_writer_wins() {
    let fixture = prepare_fixture();
    let scope = issue_scope(&fixture, "SYN-PATTERN-001", 3);
    let first = request(
        &fixture,
        scope.clone(),
        "competing-writer-first-1",
        IssueDisposition::Waived,
        None,
    );
    let second = request(
        &fixture,
        scope.clone(),
        "competing-writer-second-1",
        IssueDisposition::Waived,
        None,
    );
    let barrier = Arc::new(Barrier::new(2));
    let handles = [first, second].map(|request| {
        let barrier = Arc::clone(&barrier);
        let service = decision_service(&fixture);
        std::thread::spawn(move || {
            barrier.wait();
            service.apply(&request)
        })
    });
    let results = handles.map(|handle| handle.join().expect("competing writer thread"));
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let conflicts = results
        .iter()
        .filter_map(|result| result.as_ref().err())
        .collect::<Vec<_>>();
    assert_eq!(conflicts.len(), 1);
    assert!(
        matches!(
            conflicts[0].code(),
            "DECISION_CONFLICT" | "WORKSPACE_LOCKED"
        ),
        "the losing writer must be rejected by the issue-head check or safe writer exclusion: {}",
        conflicts[0]
    );

    let connection = open_database(&fixture);
    assert_eq!(table_count(&connection, "decision_attempts"), 1);
    assert_eq!(table_count(&connection, "issue_decisions"), 1);
    let generation: i64 = connection
        .query_row(
            "SELECT generation FROM issue_decisions
             WHERE validation_run_id = ?1 AND issue_id = ?2",
            params![fixture.validation_run_id, scope.issue_id],
            |row| row.get(0),
        )
        .expect("read single committed competing-writer head");
    assert_eq!(generation, 1);
    drop(connection);
    let integrity = integrity_check(&fixture.workspace).expect("replay competing-writer result");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn idempotency_conflicts_supersession_and_reevaluation_outcomes_fail_closed() {
    let fixture = prepare_fixture();
    let inherited_before = inherited_snapshot(&fixture);
    let service = decision_service(&fixture);
    let scope = issue_scope(&fixture, "SYN-UNIQUE-001", 3);

    let accepted_request = request(
        &fixture,
        scope.clone(),
        "chain-accepted-1",
        IssueDisposition::AcceptedAsIs,
        None,
    );
    let initial_effective_hash = accepted_request
        .concurrency
        .expected_effective_value_hash
        .clone()
        .expect("accepted-as-is effective hash");
    let accepted = service
        .apply(&accepted_request)
        .expect("commit generation-one accepted-as-is decision");
    let replay = service
        .apply(&accepted_request)
        .expect("return exact idempotent replay");
    assert_eq!(replay.data.decision_id, accepted.data.decision_id);
    assert!(replay.data.idempotent_replay);
    assert_eq!(
        table_count(&open_database(&fixture), "decision_attempts"),
        1
    );

    let mut conflicting_reuse = accepted_request.clone();
    conflicting_reuse.reason.push_str(" changed bytes");
    expect_error(service.apply(&conflicting_reuse), "DECISION_CONFLICT");
    assert_eq!(
        table_count(&open_database(&fixture), "decision_attempts"),
        1
    );

    let mut stale_head = request(
        &fixture,
        scope.clone(),
        "chain-stale-head-1",
        IssueDisposition::Waived,
        None,
    );
    stale_head.concurrency.expected_active_decision_id = None;
    stale_head.concurrency.expected_active_decision_generation = 0;
    expect_error(service.apply(&stale_head), "DECISION_CONFLICT");

    let residual_request = request(
        &fixture,
        scope.clone(),
        "chain-residual-correction-1",
        IssueDisposition::Corrected,
        Some(replacement("SYNTHETIC-DUPLICATE")),
    );
    let residual = service
        .apply(&residual_request)
        .expect("commit a correction whose source issue remains residual");
    assert!(residual.data.original_issue_present);
    assert!(residual.data.residual_issue_count > 0);
    let residual_replay = service
        .apply(&residual_request)
        .expect("replay the exact completed correction idempotently");
    assert_eq!(residual_replay.data.decision_id, residual.data.decision_id);
    assert!(residual_replay.data.idempotent_replay);
    assert_eq!(
        table_count(&open_database(&fixture), "correction_overlays"),
        1
    );
    assert_eq!(
        table_count(&open_database(&fixture), "correction_effectivations"),
        1
    );
    let residual_overlay = residual
        .data
        .correction_overlay_id
        .clone()
        .expect("residual overlay ID");

    let after_residual = issue_detail(&fixture, &scope);
    let residual_effective_hash = after_residual
        .data
        .concurrency
        .expected_effective_value_hash
        .clone();
    assert_ne!(residual_effective_hash, initial_effective_hash);

    let mut stale_value = request(
        &fixture,
        scope.clone(),
        "chain-stale-value-1",
        IssueDisposition::Corrected,
        Some(replacement("UNIQUE-STALE")),
    );
    stale_value.concurrency.expected_effective_value_hash = Some(initial_effective_hash);
    expect_error(service.apply(&stale_value), "DECISION_CONFLICT");

    let mut fork = request(
        &fixture,
        scope.clone(),
        "chain-fork-1",
        IssueDisposition::Waived,
        None,
    );
    fork.concurrency.expected_active_decision_id = Some(accepted.data.decision_id.clone());
    fork.concurrency.expected_active_decision_generation = 1;
    expect_error(service.apply(&fork), "DECISION_CONFLICT");

    let resolved = service
        .apply(&request(
            &fixture,
            scope.clone(),
            "chain-resolved-correction-1",
            IssueDisposition::Corrected,
            Some(replacement("UNIQUE-ROW-THREE")),
        ))
        .expect("append a resolving correction over the prior effective value");
    assert!(!resolved.data.original_issue_present);
    let resolved_overlay = resolved
        .data
        .correction_overlay_id
        .clone()
        .expect("resolved overlay ID");

    let row_four_duplicate_scope = issue_scope(&fixture, "SYN-UNIQUE-001", 4);
    let new_issue_scope = issue_scope(&fixture, "SYN-CONDITIONAL-001", 4);
    let creates_new = service
        .apply(&request(
            &fixture,
            new_issue_scope,
            "chain-new-issue-correction-1",
            IssueDisposition::Corrected,
            Some(replacement("東京組織")),
        ))
        .expect("commit correction that resolves one issue and creates a new warning");
    assert!(!creates_new.data.original_issue_present);
    assert_eq!(creates_new.data.new_issue_count, 1);
    assert_eq!(
        creates_new.data.residual_issue_count, 3,
        "later re-evaluation must retain already-effective overlays in its full scope"
    );
    let row_four_duplicate_outcome: String = open_database(&fixture)
        .query_row(
            "SELECT outcome FROM reevaluation_issues
             WHERE reevaluation_id = ?1 AND issue_id = ?2",
            params![
                creates_new
                    .data
                    .reevaluation_id
                    .as_deref()
                    .expect("new-issue re-evaluation ID"),
                row_four_duplicate_scope.issue_id
            ],
            |row| row.get(0),
        )
        .expect("read prior correction's cross-row uniqueness outcome");
    assert_eq!(row_four_duplicate_outcome, "RESIDUAL");
    let row_three_duplicate_outcome: String = open_database(&fixture)
        .query_row(
            "SELECT outcome FROM reevaluation_issues
             WHERE reevaluation_id = ?1 AND issue_id = ?2",
            params![
                creates_new
                    .data
                    .reevaluation_id
                    .as_deref()
                    .expect("new-issue re-evaluation ID"),
                scope.issue_id
            ],
            |row| row.get(0),
        )
        .expect("read already-effective cross-row correction outcome");
    assert_eq!(row_three_duplicate_outcome, "RESOLVED");

    let history = read_service(&fixture)
        .decision_history(
            &fixture.job_id,
            &fixture.validation_run_id,
            &scope.issue_id,
            &Default::default(),
        )
        .expect("read complete supersession chain");
    assert_eq!(history.items.len(), 3);
    assert_eq!(history.items[0].generation, 1);
    assert_eq!(history.items[1].generation, 2);
    assert_eq!(history.items[2].generation, 3);
    assert_eq!(
        history.items[1].prior_decision_id.as_deref(),
        Some(history.items[0].decision_id.as_str())
    );
    assert_eq!(
        history.items[2].prior_decision_id.as_deref(),
        Some(history.items[1].decision_id.as_str())
    );
    assert!(!history.items[0].active);
    assert!(!history.items[1].active);
    assert!(history.items[2].active);

    let old_overlay = read_service(&fixture)
        .correction_detail(&residual_overlay)
        .expect("read superseded overlay");
    assert!(!old_overlay.data.active);
    assert_eq!(
        old_overlay.data.superseded_by_overlay_id.as_deref(),
        Some(resolved_overlay.as_str())
    );
    let current_overlay = read_service(&fixture)
        .correction_detail(&resolved_overlay)
        .expect("read active overlay");
    assert!(current_overlay.data.active);
    assert!(current_overlay.data.superseded_by_overlay_id.is_none());

    let connection = open_database(&fixture);
    let chain = {
        let mut statement = connection
            .prepare(
                "SELECT decision_generation, original_value_hash,
                        prior_effective_value_hash, effective_value_hash
                 FROM correction_effectivations
                 JOIN issue_decisions USING (decision_id)
                 WHERE validation_run_id = ?1 AND issue_id = ?2
                 ORDER BY decision_generation",
            )
            .expect("prepare effectivation chain query");
        statement
            .query_map(params![fixture.validation_run_id, scope.issue_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .expect("query effectivation chain")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect effectivation chain")
    };
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].0, 2);
    assert_eq!(chain[1].0, 3);
    assert_eq!(chain[0].1, chain[1].1, "original identity changed");
    assert_eq!(
        chain[1].2, chain[0].3,
        "supersession did not bind the prior effective hash"
    );
    drop(connection);

    assert_eq!(inherited_snapshot(&fixture), inherited_before);
    let integrity = integrity_check(&fixture.workspace).expect("replay supersession evidence");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn interrupted_reevaluation_stays_inert_recovers_idempotently_and_reruns_safely() {
    let fixture = prepare_fixture();
    let inherited_before = inherited_snapshot(&fixture);
    let service = decision_service(&fixture);
    let scope = issue_scope(&fixture, "SYN-CONDITIONAL-001", 4);
    let interrupted_request = request(
        &fixture,
        scope.clone(),
        "interrupted-correction-1",
        IssueDisposition::Corrected,
        Some(replacement("Synthetic Organization")),
    );
    expect_error(
        service.apply_with_control(
            &interrupted_request,
            &ValidationControl {
                cancel_file: None,
                cancel_after_batches: Some(1),
            },
        ),
        "COOPERATIVE_CANCELLED",
    );

    let connection = open_database(&fixture);
    assert_eq!(table_count(&connection, "decision_attempts"), 1);
    assert_eq!(table_count(&connection, "correction_overlays"), 1);
    assert_eq!(table_count(&connection, "reevaluation_runs"), 1);
    assert_eq!(table_count(&connection, "issue_decisions"), 0);
    assert_eq!(table_count(&connection, "correction_effectivations"), 0);
    let terminal: (String, Option<String>, Option<String>, i64, i64) = connection
        .query_row(
            "SELECT state, result_hash, terminal_hash, issue_count, rows_processed
             FROM reevaluation_runs",
            [],
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
        .expect("read interrupted re-evaluation terminal evidence");
    assert_eq!(terminal.0, "INTERRUPTED");
    assert!(terminal.1.is_none());
    assert_eq!(terminal.2.as_deref().map(str::len), Some(64));
    assert_eq!(terminal.3, 0);
    assert_eq!(terminal.4, 0);
    let final_attempt_state: String = connection
        .query_row(
            "SELECT state FROM decision_attempt_events ORDER BY sequence DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("read interrupted decision-attempt state");
    assert_eq!(final_attempt_state, "REEVALUATION_INTERRUPTED");
    let interrupted_evidence: (String, String, String, String, String) = connection
        .query_row(
            "SELECT a.attempt_id,o.overlay_id,r.reevaluation_id,
                    r.error_code,r.error_message
             FROM decision_attempts a
             JOIN correction_overlays o ON o.decision_attempt_id=a.attempt_id
             JOIN reevaluation_runs r ON r.decision_attempt_id=a.attempt_id",
            [],
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
        .expect("read interrupted attempt identities and diagnostics");
    drop(connection);

    let reads = read_service(&fixture);
    let interrupted_queue = reads
        .decision_queue(
            &fixture.job_id,
            &fixture.validation_run_id,
            &DecisionQueueFilters {
                reevaluation_state: Some("INTERRUPTED".to_owned()),
                ..DecisionQueueFilters::default()
            },
            &Default::default(),
        )
        .expect("read value-free interrupted attempt from the governed queue");
    assert_eq!(interrupted_queue.items.len(), 1);
    let queue_attempt = interrupted_queue.items[0]
        .latest_attempt
        .as_ref()
        .expect("interrupted queue item exposes lifecycle evidence");
    assert_eq!(queue_attempt.decision_attempt_id, interrupted_evidence.0);
    assert_eq!(
        queue_attempt.correction_overlay_id.as_deref(),
        Some(interrupted_evidence.1.as_str())
    );
    assert_eq!(queue_attempt.reevaluation_state, "INTERRUPTED");
    assert_eq!(queue_attempt.lifecycle_outcome, "INTERRUPTED");
    assert!(!queue_attempt.active);
    let queue_json = serde_json::to_value(&interrupted_queue).expect("serialize decision queue");
    assert_read_contract(&queue_json, "schemas/read/decision-queue-page.schema.json");
    let broad_queue = queue_json.to_string();
    assert!(!broad_queue.contains("Synthetic Organization"));
    assert!(!broad_queue.contains(&interrupted_request.reason));

    let counts = reads
        .decision_counts(&fixture.job_id, &fixture.validation_run_id)
        .expect("count interrupted attempt state");
    assert_eq!(
        counts
            .groups
            .by_reevaluation_state
            .items
            .iter()
            .find(|item| item.key == "INTERRUPTED")
            .map(|item| item.count),
        Some(1)
    );

    let interrupted_issue = issue_detail(&fixture, &scope);
    let issue_attempt = interrupted_issue
        .data
        .latest_attempt
        .as_ref()
        .expect("exact issue detail exposes the interrupted attempt");
    assert_eq!(issue_attempt.decision_attempt_id, interrupted_evidence.0);
    assert_eq!(issue_attempt.reevaluation_state, "INTERRUPTED");
    assert!(!issue_attempt.active);
    assert_read_contract(
        &serde_json::to_value(&interrupted_issue).expect("serialize exact issue detail"),
        "schemas/read/decision-aware-issue-detail.schema.json",
    );

    let interrupted_overlay = reads
        .correction_detail(&interrupted_evidence.1)
        .expect("read inert interrupted overlay exactly");
    assert_eq!(
        interrupted_overlay.data.decision_attempt_id,
        interrupted_evidence.0
    );
    assert!(interrupted_overlay.data.decision_id.is_none());
    assert_eq!(
        interrupted_overlay.data.reevaluation_id.as_deref(),
        Some(interrupted_evidence.2.as_str())
    );
    assert_eq!(
        interrupted_overlay.data.attempt_state,
        "REEVALUATION_INTERRUPTED"
    );
    assert_eq!(interrupted_overlay.data.reevaluation_state, "INTERRUPTED");
    assert_eq!(interrupted_overlay.data.lifecycle_outcome, "INTERRUPTED");
    assert_eq!(
        interrupted_overlay.data.error_code.as_deref(),
        Some(interrupted_evidence.3.as_str())
    );
    assert_eq!(
        interrupted_overlay.data.error_message.as_deref(),
        Some(interrupted_evidence.4.as_str())
    );
    assert_eq!(
        interrupted_overlay.data.overlay_value.as_deref(),
        Some("Synthetic Organization")
    );
    assert!(interrupted_overlay.data.effective_canonical_value.is_none());
    assert!(interrupted_overlay.data.effective_value_hash.is_none());
    assert!(interrupted_overlay.data.applied_at.is_none());
    assert!(!interrupted_overlay.data.active);
    assert_read_contract(
        &serde_json::to_value(&interrupted_overlay).expect("serialize interrupted overlay"),
        "schemas/read/correction-overlay-detail.schema.json",
    );

    let counts_before_retry = {
        let connection = open_database(&fixture);
        (
            table_count(&connection, "decision_attempts"),
            table_count(&connection, "decision_attempt_events"),
            table_count(&connection, "correction_overlays"),
            table_count(&connection, "reevaluation_runs"),
        )
    };
    expect_error(service.apply(&interrupted_request), "DECISION_CONFLICT");
    let counts_after_retry = {
        let connection = open_database(&fixture);
        (
            table_count(&connection, "decision_attempts"),
            table_count(&connection, "decision_attempt_events"),
            table_count(&connection, "correction_overlays"),
            table_count(&connection, "reevaluation_runs"),
        )
    };
    assert_eq!(counts_after_retry, counts_before_retry);

    let first_recovery = recover_workspace(&fixture.workspace).expect("first recovery pass");
    let second_recovery = recover_workspace(&fixture.workspace).expect("second recovery pass");
    assert_eq!(first_recovery.recovered_jobs, 0);
    assert_eq!(second_recovery.recovered_jobs, 0);
    assert_eq!(
        table_count(&open_database(&fixture), "decision_attempts"),
        counts_before_retry.0
    );
    let detail = issue_detail(&fixture, &scope);
    assert!(detail.data.active_decision.is_none());
    assert!(detail.data.field_context.active_overlay_id.is_none());
    assert_eq!(
        detail
            .data
            .latest_attempt
            .as_ref()
            .map(|attempt| attempt.reevaluation_state.as_str()),
        Some("INTERRUPTED")
    );

    let completed = service
        .apply(&request(
            &fixture,
            scope.clone(),
            "interrupted-correction-safe-rerun-1",
            IssueDisposition::Corrected,
            Some(replacement("Synthetic Organization")),
        ))
        .expect("safe new-idempotency rerun after inert interruption");
    assert_effective(&completed, "CORRECTED");
    assert_eq!(table_count(&open_database(&fixture), "issue_decisions"), 1);
    assert_eq!(
        table_count(&open_database(&fixture), "correction_effectivations"),
        1
    );
    let completed_detail = issue_detail(&fixture, &scope);
    let completed_attempt = completed_detail
        .data
        .latest_attempt
        .expect("successful rerun becomes the latest attempt");
    assert_eq!(completed_attempt.reevaluation_state, "COMPLETE");
    assert_eq!(completed_attempt.lifecycle_outcome, "EFFECTIVE");
    assert!(completed_attempt.active);
    let retained_interrupted_overlay = read_service(&fixture)
        .correction_detail(&interrupted_evidence.1)
        .expect("interrupted historical overlay remains exactly inspectable");
    assert!(retained_interrupted_overlay.data.decision_id.is_none());
    assert_eq!(
        retained_interrupted_overlay.data.lifecycle_outcome,
        "INTERRUPTED"
    );
    assert!(!retained_interrupted_overlay.data.active);
    assert_eq!(inherited_snapshot(&fixture), inherited_before);
    let integrity = integrity_check(&fixture.workspace).expect("replay interruption and rerun");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn bound_exceeded_reevaluation_stays_inert_is_inspectable_and_reruns_safely() {
    let fixture = prepare_fixture_with_maximum_unique_keys(2);
    let inherited_before = inherited_snapshot(&fixture);
    let service = decision_service(&fixture);
    let scope = issue_scope(&fixture, "SYN-UNIQUE-001", 3);
    let bounded_request = request(
        &fixture,
        scope.clone(),
        "bound-exceeded-correction-1",
        IssueDisposition::Corrected,
        Some(replacement("UNIQUE-ROW-THREE")),
    );
    let error = expect_error(service.apply(&bounded_request), "VALIDATION_BOUND_EXCEEDED");
    assert!(
        error.to_string().contains("unique/relationship key state"),
        "unexpected bound diagnostic: {error}"
    );

    let connection = open_database(&fixture);
    assert_eq!(table_count(&connection, "decision_attempts"), 1);
    assert_eq!(table_count(&connection, "correction_overlays"), 1);
    assert_eq!(table_count(&connection, "reevaluation_runs"), 1);
    assert_eq!(table_count(&connection, "reevaluation_issues"), 0);
    assert_eq!(table_count(&connection, "issue_decisions"), 0);
    assert_eq!(table_count(&connection, "correction_effectivations"), 0);
    let terminal: BoundExceededEvidence = connection
        .query_row(
            "SELECT r.state,r.result_json,r.result_hash,r.terminal_hash,
                    r.issue_count,r.rows_processed,r.error_code,r.error_message,
                    a.attempt_id,o.overlay_id
             FROM reevaluation_runs r
             JOIN decision_attempts a ON a.attempt_id=r.decision_attempt_id
             JOIN correction_overlays o ON o.overlay_id=r.correction_overlay_id",
            [],
            |row| {
                Ok(BoundExceededEvidence {
                    state: row.get(0)?,
                    result_json: row.get(1)?,
                    result_hash: row.get(2)?,
                    terminal_hash: row.get(3)?,
                    issue_count: row.get(4)?,
                    rows_processed: row.get(5)?,
                    error_code: row.get(6)?,
                    error_message: row.get(7)?,
                    attempt_id: row.get(8)?,
                    overlay_id: row.get(9)?,
                })
            },
        )
        .expect("read bound-exceeded terminal evidence");
    assert_eq!(terminal.state, "BOUND_EXCEEDED");
    assert!(terminal.result_json.is_none());
    assert!(terminal.result_hash.is_none());
    assert_eq!(terminal.terminal_hash.as_deref().map(str::len), Some(64));
    assert_eq!(terminal.issue_count, 0);
    assert_eq!(terminal.rows_processed, 0);
    assert_eq!(terminal.error_code, "VALIDATION_BOUND_EXCEEDED");
    assert!(
        terminal
            .error_message
            .contains("unique/relationship key state")
    );
    let attempt_id = terminal.attempt_id;
    let overlay_id = terminal.overlay_id;
    let final_attempt_state: String = connection
        .query_row(
            "SELECT state FROM decision_attempt_events
             WHERE attempt_id=?1 ORDER BY sequence DESC LIMIT 1",
            [&attempt_id],
            |row| row.get(0),
        )
        .expect("read bound-exceeded attempt state");
    assert_eq!(final_attempt_state, "REEVALUATION_BOUND_EXCEEDED");
    let reevaluation_states = {
        let mut statement = connection
            .prepare("SELECT state FROM reevaluation_events ORDER BY sequence")
            .expect("prepare bound-exceeded event query");
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query bound-exceeded events")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect bound-exceeded events")
    };
    assert_eq!(
        reevaluation_states,
        ["PLANNED", "RUNNING", "BOUND_EXCEEDED"]
    );
    drop(connection);

    let reads = read_service(&fixture);
    let bounded_queue = reads
        .decision_queue(
            &fixture.job_id,
            &fixture.validation_run_id,
            &DecisionQueueFilters {
                reevaluation_state: Some("BOUND_EXCEEDED".to_owned()),
                ..DecisionQueueFilters::default()
            },
            &Default::default(),
        )
        .expect("read value-free bound-exceeded attempt from the governed queue");
    assert_eq!(bounded_queue.items.len(), 1);
    let queue_attempt = bounded_queue.items[0]
        .latest_attempt
        .as_ref()
        .expect("bound-exceeded queue item exposes lifecycle evidence");
    assert_eq!(queue_attempt.decision_attempt_id, attempt_id);
    assert_eq!(queue_attempt.reevaluation_state, "BOUND_EXCEEDED");
    assert_eq!(queue_attempt.lifecycle_outcome, "FAILED");
    assert!(!queue_attempt.active);
    let queue_json = serde_json::to_value(&bounded_queue).expect("serialize bounded queue");
    assert_read_contract(&queue_json, "schemas/read/decision-queue-page.schema.json");
    let broad_queue = queue_json.to_string();
    assert!(!broad_queue.contains("UNIQUE-ROW-THREE"));
    assert!(!broad_queue.contains(&bounded_request.reason));

    let counts = reads
        .decision_counts(&fixture.job_id, &fixture.validation_run_id)
        .expect("count bound-exceeded attempt state");
    assert_eq!(
        counts
            .groups
            .by_reevaluation_state
            .items
            .iter()
            .find(|item| item.key == "BOUND_EXCEEDED")
            .map(|item| item.count),
        Some(1)
    );
    let bounded_issue = issue_detail(&fixture, &scope);
    assert!(bounded_issue.data.active_decision.is_none());
    assert!(bounded_issue.data.field_context.active_overlay_id.is_none());
    let issue_attempt = bounded_issue
        .data
        .latest_attempt
        .as_ref()
        .expect("exact issue detail exposes the bound-exceeded attempt");
    assert_eq!(issue_attempt.decision_attempt_id, attempt_id);
    assert_eq!(issue_attempt.reevaluation_state, "BOUND_EXCEEDED");
    assert_eq!(issue_attempt.lifecycle_outcome, "FAILED");
    assert!(!issue_attempt.active);
    assert_read_contract(
        &serde_json::to_value(&bounded_issue).expect("serialize bound-exceeded issue detail"),
        "schemas/read/decision-aware-issue-detail.schema.json",
    );

    let bounded_overlay = reads
        .correction_detail(&overlay_id)
        .expect("read inert bound-exceeded overlay exactly");
    assert_eq!(bounded_overlay.data.decision_attempt_id, attempt_id);
    assert!(bounded_overlay.data.decision_id.is_none());
    assert_eq!(
        bounded_overlay.data.attempt_state,
        "REEVALUATION_BOUND_EXCEEDED"
    );
    assert_eq!(bounded_overlay.data.reevaluation_state, "BOUND_EXCEEDED");
    assert_eq!(bounded_overlay.data.lifecycle_outcome, "FAILED");
    assert_eq!(
        bounded_overlay.data.error_code.as_deref(),
        Some("VALIDATION_BOUND_EXCEEDED")
    );
    assert!(
        bounded_overlay
            .data
            .error_message
            .as_deref()
            .is_some_and(|message| message.contains("unique/relationship key state"))
    );
    assert_eq!(
        bounded_overlay.data.overlay_value.as_deref(),
        Some("UNIQUE-ROW-THREE")
    );
    assert!(bounded_overlay.data.effective_canonical_value.is_none());
    assert!(bounded_overlay.data.effective_value_hash.is_none());
    assert!(bounded_overlay.data.applied_at.is_none());
    assert!(!bounded_overlay.data.active);
    assert_read_contract(
        &serde_json::to_value(&bounded_overlay).expect("serialize bound-exceeded overlay"),
        "schemas/read/correction-overlay-detail.schema.json",
    );

    let inert_counts = {
        let connection = open_database(&fixture);
        (
            table_count(&connection, "decision_attempts"),
            table_count(&connection, "decision_attempt_events"),
            table_count(&connection, "correction_overlays"),
            table_count(&connection, "reevaluation_runs"),
            table_count(&connection, "issue_decisions"),
            table_count(&connection, "correction_effectivations"),
        )
    };
    let replay_error = expect_error(service.apply(&bounded_request), "DECISION_CONFLICT");
    assert!(
        replay_error
            .to_string()
            .contains("inert incomplete evidence"),
        "unexpected exact bound replay diagnostic: {replay_error}"
    );
    let counts_after_replay = {
        let connection = open_database(&fixture);
        (
            table_count(&connection, "decision_attempts"),
            table_count(&connection, "decision_attempt_events"),
            table_count(&connection, "correction_overlays"),
            table_count(&connection, "reevaluation_runs"),
            table_count(&connection, "issue_decisions"),
            table_count(&connection, "correction_effectivations"),
        )
    };
    assert_eq!(counts_after_replay, inert_counts);

    let first_recovery = recover_workspace(&fixture.workspace).expect("recover bounded workspace");
    let second_recovery =
        recover_workspace(&fixture.workspace).expect("repeat bounded workspace recovery");
    assert_eq!(first_recovery.recovered_decision_attempts, 0);
    assert_eq!(second_recovery.recovered_decision_attempts, 0);

    let completed = service
        .apply(&request(
            &fixture,
            scope,
            "bound-exceeded-safe-rerun-1",
            IssueDisposition::Corrected,
            Some(replacement("SYNTHETIC-DUPLICATE")),
        ))
        .expect("complete a bounded-safe new-key correction after inert bound evidence");
    assert_effective(&completed, "CORRECTED");
    assert!(completed.data.original_issue_present);
    assert_eq!(
        table_count(&open_database(&fixture), "decision_attempts"),
        2
    );
    assert_eq!(table_count(&open_database(&fixture), "issue_decisions"), 1);
    assert_eq!(
        table_count(&open_database(&fixture), "correction_effectivations"),
        1
    );
    let retained = read_service(&fixture)
        .correction_detail(&overlay_id)
        .expect("retain failed bound overlay as inert inspectable history");
    assert!(retained.data.decision_id.is_none());
    assert_eq!(retained.data.reevaluation_state, "BOUND_EXCEEDED");
    assert_eq!(retained.data.lifecycle_outcome, "FAILED");
    assert!(!retained.data.active);
    assert_eq!(inherited_snapshot(&fixture), inherited_before);
    let integrity = integrity_check(&fixture.workspace).expect("replay bound and retry evidence");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[ignore = "subprocess crash driver; invoked only by the hard-abort parent regression"]
fn hard_abort_decision_after_overlay_child_driver() {
    let workspace = PathBuf::from(
        std::env::var("MIGRATION_STREAM_TEST_DECISION_WORKSPACE")
            .expect("hard-abort child workspace"),
    );
    let request: IssueDecisionRequest = serde_json::from_str(
        &std::env::var("MIGRATION_STREAM_TEST_DECISION_REQUEST_JSON")
            .expect("hard-abort child serialized request"),
    )
    .expect("parse exact hard-abort child request");
    let result = DecisionService::new(
        workspace,
        project_path("schemas/migration-object-profile.schema.json"),
    )
    .apply(&request);
    panic!("crash injection returned instead of aborting: {result:?}");
}

#[test]
#[allow(clippy::too_many_lines)]
fn hard_abort_after_overlay_recovers_once_stays_inert_and_reruns_with_a_new_key() {
    let fixture = prepare_fixture();
    let inherited_before = inherited_snapshot(&fixture);
    let scope = issue_scope(&fixture, "SYN-CONDITIONAL-001", 4);
    let interrupted_request = request(
        &fixture,
        scope.clone(),
        "hard-abort-correction-1",
        IssueDisposition::Corrected,
        Some(replacement("Synthetic Organization")),
    );
    let executable = std::env::current_exe().expect("resolve current integration-test executable");
    let child = Command::new(executable)
        .arg("--ignored")
        .arg("--exact")
        .arg("hard_abort_decision_after_overlay_child_driver")
        .arg("--nocapture")
        .env("MIGRATION_STREAM_TEST_CRASH_DECISION_AFTER_OVERLAY", "1")
        .env(
            "MIGRATION_STREAM_TEST_DECISION_WORKSPACE",
            &fixture.workspace,
        )
        .env(
            "MIGRATION_STREAM_TEST_DECISION_REQUEST_JSON",
            serde_json::to_string(&interrupted_request)
                .expect("serialize exact hard-abort request"),
        )
        .output()
        .expect("spawn crash-injected decision subprocess");
    assert!(
        !child.status.success(),
        "crash-injected child unexpectedly succeeded: stdout={} stderr={}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    #[cfg(unix)]
    assert!(
        child.status.code().is_none(),
        "hard-abort child exited normally instead of receiving an abort signal: {:?}",
        child.status
    );

    let (attempt_id, overlay_id, last_state) = {
        let connection = open_database(&fixture);
        assert_eq!(table_count(&connection, "decision_attempts"), 1);
        assert_eq!(table_count(&connection, "correction_overlays"), 1);
        assert_eq!(table_count(&connection, "reevaluation_runs"), 0);
        assert_eq!(table_count(&connection, "issue_decisions"), 0);
        assert_eq!(table_count(&connection, "correction_effectivations"), 0);
        connection
            .query_row(
                "SELECT a.attempt_id, o.overlay_id,
                        (SELECT e.state FROM decision_attempt_events e
                         WHERE e.attempt_id = a.attempt_id
                         ORDER BY e.sequence DESC LIMIT 1)
                 FROM decision_attempts a
                 JOIN correction_overlays o ON o.decision_attempt_id = a.attempt_id",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .expect("read hard-aborted inert overlay evidence")
    };
    assert_eq!(last_state, "REEVALUATION_RUNNING");

    let first_recovery =
        recover_workspace(&fixture.workspace).expect("recover hard-aborted decision attempt");
    assert_eq!(first_recovery.recovered_jobs, 0);
    assert_eq!(first_recovery.recovered_profiling_runs, 0);
    assert_eq!(first_recovery.recovered_validation_runs, 0);
    assert_eq!(first_recovery.recovered_decision_attempts, 1);
    let repeated_recovery =
        recover_workspace(&fixture.workspace).expect("repeat decision-attempt recovery");
    assert_eq!(repeated_recovery.recovered_jobs, 0);
    assert_eq!(repeated_recovery.recovered_profiling_runs, 0);
    assert_eq!(repeated_recovery.recovered_validation_runs, 0);
    assert_eq!(repeated_recovery.recovered_decision_attempts, 0);

    let recovered_counts = {
        let connection = open_database(&fixture);
        let terminal: (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
            String,
        ) = connection
            .query_row(
                "SELECT r.state, r.result_json, r.result_hash, r.terminal_hash,
                        r.error_code,
                        (SELECT e.state FROM decision_attempt_events e
                         WHERE e.attempt_id = r.decision_attempt_id
                         ORDER BY e.sequence DESC LIMIT 1)
                 FROM reevaluation_runs r
                 WHERE r.decision_attempt_id = ?1",
                [&attempt_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .expect("read recovered decision terminal evidence");
        assert_eq!(terminal.0, "INTERRUPTED");
        assert!(terminal.1.is_none());
        assert!(terminal.2.is_none());
        assert_eq!(terminal.3.as_deref().map(str::len), Some(64));
        assert_eq!(terminal.4, "INTERRUPTED_RECOVERED");
        assert_eq!(terminal.5, "REEVALUATION_INTERRUPTED");
        assert_eq!(table_count(&connection, "decision_attempts"), 1);
        assert_eq!(table_count(&connection, "correction_overlays"), 1);
        assert_eq!(table_count(&connection, "reevaluation_runs"), 1);
        assert_eq!(table_count(&connection, "issue_decisions"), 0);
        assert_eq!(table_count(&connection, "correction_effectivations"), 0);
        (
            table_count(&connection, "decision_attempts"),
            table_count(&connection, "decision_attempt_events"),
            table_count(&connection, "correction_overlays"),
            table_count(&connection, "reevaluation_runs"),
        )
    };

    let interrupted_overlay = read_service(&fixture)
        .correction_detail(&overlay_id)
        .expect("read recovered inert overlay");
    assert_eq!(interrupted_overlay.data.decision_attempt_id, attempt_id);
    assert!(interrupted_overlay.data.decision_id.is_none());
    assert_eq!(
        interrupted_overlay.data.attempt_state,
        "REEVALUATION_INTERRUPTED"
    );
    assert_eq!(interrupted_overlay.data.reevaluation_state, "INTERRUPTED");
    assert_eq!(interrupted_overlay.data.lifecycle_outcome, "INTERRUPTED");
    assert!(interrupted_overlay.data.effective_canonical_value.is_none());
    assert!(interrupted_overlay.data.effective_value_hash.is_none());
    assert!(interrupted_overlay.data.applied_at.is_none());
    assert!(!interrupted_overlay.data.active);
    let interrupted_detail = issue_detail(&fixture, &scope);
    assert!(interrupted_detail.data.active_decision.is_none());
    assert!(
        interrupted_detail
            .data
            .field_context
            .active_overlay_id
            .is_none()
    );

    let replay_error = expect_error(
        decision_service(&fixture).apply(&interrupted_request),
        "DECISION_CONFLICT",
    );
    assert!(
        replay_error
            .to_string()
            .contains("inert incomplete evidence"),
        "unexpected exact replay diagnostic: {replay_error}"
    );
    let counts_after_replay = {
        let connection = open_database(&fixture);
        (
            table_count(&connection, "decision_attempts"),
            table_count(&connection, "decision_attempt_events"),
            table_count(&connection, "correction_overlays"),
            table_count(&connection, "reevaluation_runs"),
        )
    };
    assert_eq!(counts_after_replay, recovered_counts);

    let completed = decision_service(&fixture)
        .apply(&request(
            &fixture,
            scope.clone(),
            "hard-abort-correction-safe-rerun-1",
            IssueDisposition::Corrected,
            Some(replacement("Synthetic Organization")),
        ))
        .expect("complete a safe new-key rerun after hard-abort recovery");
    assert_effective(&completed, "CORRECTED");
    assert_ne!(
        completed.data.correction_overlay_id.as_deref(),
        Some(overlay_id.as_str())
    );
    assert_eq!(
        table_count(&open_database(&fixture), "decision_attempts"),
        2
    );
    assert_eq!(table_count(&open_database(&fixture), "issue_decisions"), 1);
    assert_eq!(
        table_count(&open_database(&fixture), "correction_effectivations"),
        1
    );
    let retained = read_service(&fixture)
        .correction_detail(&overlay_id)
        .expect("retain recovered interrupted overlay as inert history");
    assert_eq!(retained.data.lifecycle_outcome, "INTERRUPTED");
    assert!(!retained.data.active);
    assert_eq!(inherited_snapshot(&fixture), inherited_before);
    let integrity =
        integrity_check(&fixture.workspace).expect("replay hard-abort recovery evidence");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
fn explicit_empty_null_and_text_replacements_remain_distinct_and_non_destructive() {
    let fixture = prepare_fixture();
    let inherited_before = inherited_snapshot(&fixture);
    let service = decision_service(&fixture);
    let scope = issue_scope(&fixture, "SYN-PATTERN-001", 2);

    let empty = service
        .apply(&request(
            &fixture,
            scope.clone(),
            "text-operation-empty-1",
            IssueDisposition::Corrected,
            Some(CorrectionOverlayRequest {
                operation: CorrectionOperation::SetEffectiveEmpty,
                replacement_text: None,
            }),
        ))
        .expect("effectivate explicit empty text");
    let empty_overlay = read_service(&fixture)
        .correction_detail(
            empty
                .data
                .correction_overlay_id
                .as_deref()
                .expect("empty overlay ID"),
        )
        .expect("read explicit empty overlay");
    assert_eq!(empty_overlay.data.operation, "SET_EFFECTIVE_EMPTY");
    assert_eq!(empty_overlay.data.overlay_value.as_deref(), Some(""));
    assert_eq!(
        empty_overlay.data.effective_canonical_value.as_deref(),
        Some("")
    );

    let null = service
        .apply(&request(
            &fixture,
            scope.clone(),
            "text-operation-null-1",
            IssueDisposition::Corrected,
            Some(CorrectionOverlayRequest {
                operation: CorrectionOperation::SetEffectiveNull,
                replacement_text: None,
            }),
        ))
        .expect("effectivate explicit null");
    let null_overlay = read_service(&fixture)
        .correction_detail(
            null.data
                .correction_overlay_id
                .as_deref()
                .expect("null overlay ID"),
        )
        .expect("read explicit null overlay");
    assert_eq!(null_overlay.data.operation, "SET_EFFECTIVE_NULL");
    assert!(null_overlay.data.overlay_value.is_none());
    assert!(null_overlay.data.effective_canonical_value.is_none());

    let exact_text = "0007 2026-07-22 <script>literal</script>";
    let textual = service
        .apply(&request(
            &fixture,
            scope.clone(),
            "text-operation-replace-1",
            IssueDisposition::Corrected,
            Some(replacement(exact_text)),
        ))
        .expect("effectivate exact text without type inference or HTML execution");
    let textual_overlay = read_service(&fixture)
        .correction_detail(
            textual
                .data
                .correction_overlay_id
                .as_deref()
                .expect("text overlay ID"),
        )
        .expect("read exact textual overlay");
    assert_eq!(textual_overlay.data.operation, "REPLACE_EFFECTIVE_TEXT");
    assert_eq!(
        textual_overlay.data.overlay_value.as_deref(),
        Some(exact_text)
    );
    assert_eq!(
        textual_overlay.data.effective_canonical_value.as_deref(),
        Some(exact_text)
    );
    assert_eq!(
        issue_detail(&fixture, &scope)
            .data
            .field_context
            .effective_canonical_value
            .as_deref(),
        Some(exact_text)
    );

    let kinds = {
        let connection = open_database(&fixture);
        let mut statement = connection
            .prepare(
                "SELECT effective_value_kind, effective_value_text
                 FROM correction_effectivations ce
                 JOIN issue_decisions d ON d.decision_id = ce.decision_id
                 WHERE d.validation_run_id = ?1 AND d.issue_id = ?2
                 ORDER BY d.generation",
            )
            .expect("prepare exact effective-kind query");
        statement
            .query_map(params![fixture.validation_run_id, scope.issue_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .expect("query exact effective kinds")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect exact effective kinds")
    };
    assert_eq!(
        kinds,
        vec![
            ("EMPTY".to_owned(), Some(String::new())),
            ("NULL".to_owned(), None),
            ("TEXT".to_owned(), Some(exact_text.to_owned())),
        ]
    );
    assert_eq!(inherited_snapshot(&fixture), inherited_before);
    let integrity = integrity_check(&fixture.workspace).expect("replay explicit text operations");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[tokio::test]
async fn http_correction_cap_is_stricter_than_the_bounded_direct_persistence_contract() {
    const HTTP_CORRECTION_CHARACTERS: usize = 65_536;
    let fixture = prepare_fixture_with_name_maximum(Some(70_000));
    let scope = issue_scope(&fixture, "SYN-PATTERN-001", 2);
    let detail = issue_detail(&fixture, &scope);
    let command = |value: String| {
        json!({
            "schema_version": "migration-stream/write/correct-issue/v1",
            "validation_run_id": fixture.validation_run_id,
            "issue_hash": scope.issue_hash,
            "practitioner_identifier": "synthetic-practitioner-005",
            "reason": "Exercise the bounded transport/direct persistence distinction",
            "evidence_reference": null,
            "expected_active_decision_id": null,
            "expected_active_generation": 0,
            "acknowledge_not_migration_result_approval": true,
            "correction": {
                "source_file": scope.accepted_source_file,
                "source_ordinal": scope.source_ordinal,
                "entity_id": scope.canonical_entity_id,
                "field_id": scope.canonical_field_id,
                "operation": "REPLACE_EFFECTIVE_TEXT",
                "value": value,
                "expected_effective_value_hash": detail.data.concurrency.expected_effective_value_hash,
            }
        })
    };

    let schema: Value = serde_json::from_slice(
        &fs::read(project_path("schemas/write/correct-issue.schema.json"))
            .expect("read exact correction transport schema"),
    )
    .expect("parse exact correction transport schema");
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .with_pattern_options(PatternOptions::fancy_regex())
        .build(&schema)
        .expect("compile correction transport schema");
    let at_http_limit = command("x".repeat(HTTP_CORRECTION_CHARACTERS));
    assert!(
        validator.is_valid(&at_http_limit),
        "the exact HTTP correction character ceiling must remain accepted"
    );
    let above_http_limit = command("x".repeat(HTTP_CORRECTION_CHARACTERS + 1));
    assert!(
        !validator.is_valid(&above_http_limit),
        "the versioned HTTP contract must reject one character above its ceiling"
    );

    let app = build_router(
        WorkbenchConfig::new(&fixture.workspace, fixture.job_id.clone(), None)
            .with_governed_decisions(),
    )
    .expect("preflight governed workbench for transport-bound proof");
    let bytes = serde_json::to_vec(&above_http_limit).expect("serialize oversized HTTP command");
    let response = app
        .oneshot(
            HttpRequest::builder()
                .method(Method::POST)
                .uri(format!(
                    "/api/v1/issues/{}/decisions/correct",
                    scope.issue_id
                ))
                .header("host", "127.0.0.1:4173")
                .header("origin", "http://127.0.0.1:4173")
                .header("content-type", "application/json")
                .header("idempotency-key", "http-correction-over-cap-1")
                .header("x-migration-stream-intent", "governed-decision-v1")
                .header("content-length", bytes.len())
                .body(Body::from(bytes))
                .expect("build bounded HTTP request"),
        )
        .await
        .expect("infallible governed router response");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response_body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read bounded error response");
    let error: Value = serde_json::from_slice(&response_body).expect("parse transport error");
    assert_eq!(error["error"]["code"], "INVALID_DECISION_COMMAND");
    assert_eq!(
        table_count(&open_database(&fixture), "decision_attempts"),
        0
    );

    let direct_value = "x".repeat(HTTP_CORRECTION_CHARACTERS + 1);
    let direct_request = request(
        &fixture,
        scope,
        "direct-correction-above-http-cap-1",
        IssueDisposition::Corrected,
        Some(replacement(&direct_value)),
    );
    direct_request
        .validate()
        .expect("direct domain contract permits its larger bounded value");
    let direct = decision_service(&fixture)
        .apply(&direct_request)
        .expect("persist a direct bounded correction above the narrower HTTP cap");
    assert_effective(&direct, "CORRECTED");
    let canonical_lengths: (i64, i64) = open_database(&fixture)
        .query_row(
            "SELECT length(a.canonical_request_json), length(o.canonical_overlay_json)
             FROM decision_attempts a
             JOIN correction_overlays o ON o.decision_attempt_id = a.attempt_id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("measure persisted direct canonical evidence");
    assert!(canonical_lengths.0 > 65_536);
    assert!(canonical_lengths.1 > 65_536);
}

#[test]
fn canonical_decision_schemas_reject_the_domain_invalid_zero_source_ordinal() {
    let fixture = prepare_fixture();
    let corrected = decision_service(&fixture)
        .apply(&request(
            &fixture,
            issue_scope(&fixture, "SYN-PATTERN-001", 2),
            "canonical-schema-positive-ordinal-1",
            IssueDisposition::Corrected,
            Some(replacement("Synthetic Schema Parity")),
        ))
        .expect("create canonical decision and overlay evidence");
    let canonical_documents: (String, String, String, String) = open_database(&fixture)
        .query_row(
            "SELECT d.canonical_decision_json, d.decision_hash,
                    o.canonical_overlay_json, o.overlay_hash
             FROM issue_decisions d
             JOIN correction_overlays o
               ON o.overlay_id = d.correction_overlay_id
             WHERE d.decision_id = ?1",
            [&corrected.data.decision_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read persisted canonical decision documents");

    for (schema_path, canonical_json, hash_field, hash) in [
        (
            "schemas/decisions/issue-decision.schema.json",
            canonical_documents.0,
            "decisionHash",
            canonical_documents.1,
        ),
        (
            "schemas/decisions/correction-overlay.schema.json",
            canonical_documents.2,
            "overlayHash",
            canonical_documents.3,
        ),
    ] {
        let schema: Value = serde_json::from_slice(
            &fs::read(project_path(schema_path)).expect("read canonical decision schema"),
        )
        .expect("parse canonical decision schema");
        let validator = jsonschema::draft202012::options()
            .should_validate_formats(true)
            .should_ignore_unknown_formats(false)
            .with_pattern_options(PatternOptions::fancy_regex())
            .build(&schema)
            .expect("compile canonical decision schema");
        let material: Value =
            serde_json::from_str(&canonical_json).expect("parse persisted canonical document");
        let mut document = material["record"].clone();
        document[hash_field] = json!(hash);
        assert!(
            validator.is_valid(&document),
            "persisted positive-ordinal document must satisfy {schema_path}"
        );
        document["scope"]["sourceOrdinal"] = json!(0);
        assert!(
            !validator.is_valid(&document),
            "{schema_path} must reject sourceOrdinal zero exactly like the domain and database"
        );
    }
}

#[test]
fn unicode_character_schema_limits_cannot_bypass_stricter_utf8_service_limits() {
    let fixture = prepare_fixture();
    let service = decision_service(&fixture);
    let committed = service
        .apply(&request(
            &fixture,
            issue_scope(&fixture, "SYN-PATTERN-001", 2),
            "unicode-bound-canonical-baseline-1",
            IssueDisposition::Waived,
            None,
        ))
        .expect("create one valid canonical decision baseline");
    let (canonical_json, decision_hash): (String, String) = open_database(&fixture)
        .query_row(
            "SELECT canonical_decision_json, decision_hash
             FROM issue_decisions WHERE decision_id = ?1",
            [&committed.data.decision_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read valid canonical decision baseline");
    let material: Value =
        serde_json::from_str(&canonical_json).expect("parse canonical decision material");
    let mut canonical_record = material["record"].clone();
    canonical_record["decisionHash"] = json!(decision_hash);
    let schema: Value = serde_json::from_slice(
        &fs::read(project_path("schemas/decisions/issue-decision.schema.json"))
            .expect("read canonical issue-decision schema"),
    )
    .expect("parse canonical issue-decision schema");
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .with_pattern_options(PatternOptions::fancy_regex())
        .build(&schema)
        .expect("compile canonical issue-decision schema");

    let invalid_scope = issue_scope(&fixture, "SYN-PATTERN-001", 3);
    let cases = [
        ("practitionerId", "😀".repeat(256)),
        ("reason", "😀".repeat(4_096)),
        ("evidenceReference", "😀".repeat(2_048)),
    ];
    for (index, (field, oversized_utf8)) in cases.into_iter().enumerate() {
        let mut schema_instance = canonical_record.clone();
        schema_instance[field] = json!(oversized_utf8.clone());
        assert!(
            validator.is_valid(&schema_instance),
            "JSON Schema maxLength is intentionally a Unicode-character ceiling"
        );

        let mut invalid_request = request(
            &fixture,
            invalid_scope.clone(),
            &format!("unicode-byte-bound-{index}"),
            IssueDisposition::Waived,
            None,
        );
        match field {
            "practitionerId" => invalid_request.practitioner_id = oversized_utf8,
            "reason" => invalid_request.reason = oversized_utf8,
            "evidenceReference" => invalid_request.evidence_reference = Some(oversized_utf8),
            _ => unreachable!("closed Unicode-bound test field"),
        }
        expect_error(invalid_request.validate(), "DECISION_INVALID");
        expect_error(service.apply(&invalid_request), "DECISION_INVALID");
    }
    assert_eq!(
        table_count(&open_database(&fixture), "decision_attempts"),
        1,
        "character-valid but UTF-8-byte-oversized attribution must never persist"
    );
}

fn create_corrected_fixture() -> (DecisionFixture, DecisionCommandResult) {
    let fixture = prepare_fixture();
    let result = decision_service(&fixture)
        .apply(&request(
            &fixture,
            issue_scope(&fixture, "SYN-PATTERN-001", 2),
            "tamper-correction-1",
            IssueDisposition::Corrected,
            Some(replacement("Synthetic Athens")),
        ))
        .expect("create complete correction evidence for tamper proof");
    (fixture, result)
}

#[test]
#[allow(clippy::too_many_lines)]
fn tombstone_source_authority_is_excluded_from_every_governed_read_consumer() {
    let (fixture, completed) = create_corrected_fixture();
    let overlay_id = completed
        .data
        .correction_overlay_id
        .as_deref()
        .expect("completed correction overlay ID");
    let (issue_id, source_hash, stored_archive): (String, String, String) = open_database(&fixture)
        .query_row(
            "SELECT a.issue_id,j.source_hash,j.stored_archive
             FROM issue_decisions d
             JOIN decision_attempts a ON a.attempt_id=d.decision_attempt_id
             JOIN jobs j ON j.id=a.job_id
             WHERE d.decision_id=?1",
            [&completed.data.decision_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read completed correction source authority");
    let reads = read_service(&fixture);

    reads
        .decision_queue(
            &fixture.job_id,
            &fixture.validation_run_id,
            &DecisionQueueFilters::default(),
            &Default::default(),
        )
        .expect("baseline governed queue");
    reads
        .decision_counts(&fixture.job_id, &fixture.validation_run_id)
        .expect("baseline governed counts");
    reads
        .issue_detail(&fixture.job_id, &fixture.validation_run_id, &issue_id)
        .expect("baseline governed issue detail");
    reads
        .decision_history(
            &fixture.job_id,
            &fixture.validation_run_id,
            &issue_id,
            &Default::default(),
        )
        .expect("baseline governed history");
    reads
        .correction_detail(overlay_id)
        .expect("baseline governed correction detail");
    reads
        .decision_result(&completed.data.decision_id)
        .expect("baseline governed decision result");

    let accepted_archive = fixture.workspace.join(&stored_archive);
    let accepted_bytes = fs::read(&accepted_archive).expect("read accepted package bytes");
    assert_eq!(
        sha256_hex(&accepted_bytes),
        source_hash,
        "baseline package bytes differ from their accepted source hash"
    );
    let tombstone_relative = format!("packages/{source_hash}.tombstone");
    let tombstone = fixture.workspace.join(&tombstone_relative);
    fs::rename(&accepted_archive, &tombstone)
        .expect("move content-identical accepted bytes into inert tombstone namespace");
    assert!(!accepted_archive.exists());
    assert_eq!(
        sha256_hex(fs::read(&tombstone).expect("read inert tombstone bytes")),
        source_hash,
        "tombstone fixture must retain the exact accepted content hash"
    );
    open_database(&fixture)
        .execute(
            "UPDATE jobs SET stored_archive=?1 WHERE id=?2",
            params![tombstone_relative, fixture.job_id],
        )
        .expect("install test-only content-identical tombstone authority");

    expect_tombstone_read_blocked(
        reads.decision_queue(
            &fixture.job_id,
            &fixture.validation_run_id,
            &DecisionQueueFilters::default(),
            &Default::default(),
        ),
        "decision queue",
    );
    expect_tombstone_read_blocked(
        reads.decision_counts(&fixture.job_id, &fixture.validation_run_id),
        "decision counts",
    );
    expect_tombstone_read_blocked(
        reads.issue_detail(&fixture.job_id, &fixture.validation_run_id, &issue_id),
        "issue detail",
    );
    expect_tombstone_read_blocked(
        reads.decision_history(
            &fixture.job_id,
            &fixture.validation_run_id,
            &issue_id,
            &Default::default(),
        ),
        "decision history",
    );
    expect_tombstone_read_blocked(reads.correction_detail(overlay_id), "correction detail");
    expect_tombstone_read_blocked(
        reads.correction_detail_for_job(&fixture.job_id, overlay_id),
        "job-scoped correction detail",
    );
    expect_tombstone_read_blocked(
        reads.decision_result(&completed.data.decision_id),
        "decision result",
    );
    expect_tombstone_read_blocked(
        reads.decision_result_for_job(&fixture.job_id, &completed.data.decision_id),
        "job-scoped decision result",
    );
}

fn assert_integrity_tamper_blocked(
    fixture: &DecisionFixture,
    decision_id: &str,
    blocked_request: &IssueDecisionRequest,
) {
    let error = expect_error(
        read_service(fixture).decision_result(decision_id),
        "DECISION_INTEGRITY_FAILED",
    );
    assert!(
        error.to_string().contains("integrity")
            || error.to_string().contains("canonical")
            || error.to_string().contains("hash")
            || error.to_string().contains("differs"),
        "unexpected tamper diagnostic: {error}"
    );
    let service_error = expect_error(
        decision_service(fixture).apply(blocked_request),
        "DECISION_INTEGRITY_FAILED",
    );
    assert!(service_error.to_string().contains("integrity"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn structurally_rehashed_false_complete_reevaluation_fails_deterministic_engine_replay() {
    let (fixture, completed) = create_corrected_fixture();
    let clean = integrity_check(&fixture.workspace).expect("replay untampered correction");
    assert!(clean.ok, "baseline integrity issues: {:?}", clean.issues);
    let connection = open_database(&fixture);
    let target: ReevaluationIssueTamperTarget = connection
        .query_row(
            "SELECT ri.reevaluation_id, ri.issue_id,
                    ri.source_validation_run_id, ri.source_issue_id,
                    ri.severity, ri.rule_id, ri.issue_code, ri.display_message,
                    ri.entity_id, ri.field_id, ri.source_file, ri.source_ordinal,
                    ri.line_start, ri.line_end, ri.lineage_json, vi.issue_hash
             FROM reevaluation_issues ri
             JOIN validation_issues vi
               ON vi.run_id = ri.source_validation_run_id
              AND vi.issue_id = ri.source_issue_id
             WHERE ri.outcome = 'RESIDUAL'
             ORDER BY ri.issue_id LIMIT 1",
            [],
            |row| {
                Ok(ReevaluationIssueTamperTarget {
                    reevaluation_id: row.get(0)?,
                    issue_id: row.get(1)?,
                    source_validation_run_id: row.get(2)?,
                    source_issue_id: row.get(3)?,
                    severity: row.get(4)?,
                    rule_id: row.get(5)?,
                    issue_code: row.get(6)?,
                    display_message: row.get(7)?,
                    entity_id: row.get(8)?,
                    field_id: row.get(9)?,
                    source_file: row.get(10)?,
                    source_ordinal: row.get(11)?,
                    line_start: row.get(12)?,
                    line_end: row.get(13)?,
                    lineage_json: row.get(14)?,
                    engine_issue_hash: row.get(15)?,
                })
            },
        )
        .expect("select deterministic residual issue to falsify");
    assert_eq!(target.source_issue_id, target.issue_id);
    let lineage: Value =
        serde_json::from_str(&target.lineage_json).expect("parse exact issue lineage");
    let false_issue_material = json!({
        "contract": "migration-stream-reevaluation-issue/v1",
        "displayMessage": target.display_message,
        "engineIssueHash": target.engine_issue_hash,
        "entityId": target.entity_id,
        "fieldId": target.field_id,
        "issueCode": target.issue_code,
        "issueId": target.issue_id,
        "lineEnd": target.line_end,
        "lineStart": target.line_start,
        "lineage": lineage,
        "outcome": "RESOLVED",
        "reevaluationId": target.reevaluation_id,
        "ruleId": target.rule_id,
        "severity": target.severity,
        "sourceFile": target.source_file,
        "sourceIssueId": target.source_issue_id,
        "sourceOrdinal": target.source_ordinal,
        "sourceValidationRunId": target.source_validation_run_id,
    });
    let false_issue_hash =
        canonical_hash(&false_issue_material).expect("hash false but structural issue evidence");
    let issue_trigger: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'trigger' AND name = 'reevaluation_issues_no_update'",
            [],
            |row| row.get(0),
        )
        .expect("capture immutable issue trigger");
    let run_trigger: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'trigger' AND name = 'reevaluation_runs_no_update'",
            [],
            |row| row.get(0),
        )
        .expect("capture immutable run trigger");
    connection
        .execute_batch(
            "DROP TRIGGER reevaluation_issues_no_update;
             DROP TRIGGER reevaluation_runs_no_update;",
        )
        .expect("open deliberate structurally consistent tamper window");
    connection
        .execute(
            "UPDATE reevaluation_issues
             SET outcome = 'RESOLVED', issue_hash = ?1
             WHERE reevaluation_id = ?2 AND issue_id = ?3",
            params![false_issue_hash, target.reevaluation_id, target.issue_id],
        )
        .expect("seal false resolution with its matching structural hash");

    let digests = {
        let mut statement = connection
            .prepare(
                "SELECT issue_id, issue_hash, outcome
                 FROM reevaluation_issues
                 WHERE reevaluation_id = ?1 ORDER BY issue_id",
            )
            .expect("prepare rehashed result digest query");
        statement
            .query_map([&target.reevaluation_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .expect("query rehashed result digests")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect rehashed result digests")
    };
    let run: CompleteReevaluationTamperContext = connection
        .query_row(
            "SELECT identity_hash, dependency_scope_hash, source_issue_id,
                    source_validation_run_id, rows_processed, issue_count,
                    created_at, completed_at
             FROM reevaluation_runs WHERE reevaluation_id = ?1",
            [&target.reevaluation_id],
            |row| {
                Ok(CompleteReevaluationTamperContext {
                    identity_hash: row.get(0)?,
                    dependency_scope_hash: row.get(1)?,
                    source_issue_id: row.get(2)?,
                    source_validation_run_id: row.get(3)?,
                    rows_processed: row.get(4)?,
                    issue_count: row.get(5)?,
                    created_at: row.get(6)?,
                    completed_at: row.get(7)?,
                })
            },
        )
        .expect("read COMPLETE re-evaluation structural context");
    assert_eq!(
        usize::try_from(run.issue_count).expect("positive issue count"),
        digests.len()
    );
    let issue_values = digests
        .iter()
        .map(|(issue_id, issue_hash, outcome)| {
            json!({
                "issueHash": issue_hash,
                "issueId": issue_id,
                "outcome": outcome,
            })
        })
        .collect::<Vec<_>>();
    let resolved = digests
        .iter()
        .filter(|(_, _, outcome)| outcome == "RESOLVED")
        .count();
    let residual = digests
        .iter()
        .filter(|(_, _, outcome)| outcome == "RESIDUAL")
        .count();
    let new = digests
        .iter()
        .filter(|(_, _, outcome)| outcome == "NEW")
        .count();
    let false_result = json!({
        "contract": "migration-stream-reevaluation-result/v1",
        "currentIssueCount": residual + new,
        "dependencyScopeHash": run.dependency_scope_hash,
        "identityHash": run.identity_hash,
        "issues": issue_values,
        "newCount": new,
        "reevaluationId": target.reevaluation_id,
        "residualCount": residual,
        "resolvedCount": resolved,
        "rowsProcessed": run.rows_processed,
        "sourceIssueId": run.source_issue_id,
        "sourceValidationRunId": run.source_validation_run_id,
    });
    let false_result_json =
        canonical_json_string(&false_result).expect("serialize false canonical result");
    let false_result_hash = canonical_hash(&false_result).expect("hash false canonical result");
    let false_terminal = json!({
        "completedAt": run.completed_at,
        "contract": "migration-stream-reevaluation-terminal/v1",
        "createdAt": run.created_at,
        "errorCode": null,
        "errorMessage": null,
        "issues": issue_values,
        "identityHash": run.identity_hash,
        "issueCount": run.issue_count,
        "reevaluationId": target.reevaluation_id,
        "resultHash": false_result_hash,
        "rowsProcessed": run.rows_processed,
        "state": "COMPLETE",
    });
    let false_terminal_hash =
        canonical_hash(&false_terminal).expect("hash false canonical terminal");
    connection
        .execute(
            "UPDATE reevaluation_runs
             SET result_json = ?1, result_hash = ?2, terminal_hash = ?3
             WHERE reevaluation_id = ?4",
            params![
                false_result_json,
                false_result_hash,
                false_terminal_hash,
                target.reevaluation_id
            ],
        )
        .expect("seal structurally self-consistent false COMPLETE result");
    connection
        .execute_batch(&format!("{issue_trigger};\n{run_trigger};"))
        .expect("restore exact append-only triggers after deliberate tamper");
    drop(connection);

    let replay =
        integrity_check(&fixture.workspace).expect("execute deterministic integrity replay");
    assert!(
        !replay.ok,
        "false COMPLETE result unexpectedly replayed cleanly"
    );
    let diagnostic = replay.issues.join("\n");
    assert!(
        diagnostic.contains("differs from deterministic issue"),
        "failure did not reach deterministic engine comparison: {diagnostic}"
    );
    for structural_failure in [
        "binding/hash mismatch",
        "result bytes/hash mismatch",
        "terminal hash mismatch",
        "issue count differs",
    ] {
        assert!(
            !diagnostic.contains(structural_failure),
            "tamper was not structurally self-consistent: {diagnostic}"
        );
    }
    let blocked = expect_error(
        read_service(&fixture).decision_result(&completed.data.decision_id),
        "DECISION_INTEGRITY_FAILED",
    );
    assert!(blocked.to_string().contains("deterministic issue"));
}

#[test]
fn decision_overlay_reevaluation_effectivation_and_chain_tampering_fail_closed() {
    let scenarios: [(&str, &str, &str); 5] = [
        (
            "decision_attempts_no_update",
            "UPDATE decision_attempts SET practitioner_id = 'tampered-practitioner';",
            "decision-attempt attribution",
        ),
        (
            "correction_overlays_no_update",
            "UPDATE correction_overlays SET replacement_text = 'tampered-overlay';",
            "correction overlay",
        ),
        (
            "reevaluation_issues_no_update",
            "UPDATE reevaluation_issues SET display_message = display_message || ' tampered';",
            "re-evaluation issue",
        ),
        (
            "correction_effectivations_no_update",
            "UPDATE correction_effectivations SET effective_value_text = 'tampered-effective';",
            "effective value",
        ),
        (
            "reevaluation_runs_no_update",
            "UPDATE reevaluation_runs SET source_issue_id = 'tampered-source-issue';",
            "source-run binding",
        ),
    ];
    for (trigger, update, label) in scenarios {
        let (fixture, result) = create_corrected_fixture();
        let blocked_request = request(
            &fixture,
            issue_scope(&fixture, "SYN-PATTERN-001", 3),
            "tamper-must-block-new-authority",
            IssueDisposition::Waived,
            None,
        );
        let connection = open_database(&fixture);
        let trigger_sql: String = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = ?1",
                [trigger],
                |row| row.get(0),
            )
            .expect("capture exact append-only guard before test tamper");
        connection
            .execute_batch("PRAGMA foreign_keys = OFF;")
            .expect("disable FK enforcement only in deliberate tamper connection");
        connection
            .execute_batch(&format!("DROP TRIGGER {trigger};"))
            .expect("open deliberate immutable-row tamper window");
        connection
            .execute_batch(update)
            .unwrap_or_else(|error| panic!("apply {label} tamper: {error}"));
        connection
            .execute_batch(&trigger_sql)
            .expect("restore exact append-only guard after test tamper");
        drop(connection);
        assert_integrity_tamper_blocked(&fixture, &result.data.decision_id, &blocked_request);
    }

    let (fixture, first) = create_corrected_fixture();
    let scope = issue_scope(&fixture, "SYN-PATTERN-001", 2);
    let second = decision_service(&fixture)
        .apply(&request(
            &fixture,
            scope,
            "tamper-chain-second-1",
            IssueDisposition::Waived,
            None,
        ))
        .expect("create generation-two decision");
    let blocked_request = request(
        &fixture,
        issue_scope(&fixture, "SYN-PATTERN-001", 3),
        "tamper-chain-must-block-new-authority",
        IssueDisposition::Waived,
        None,
    );
    let connection = open_database(&fixture);
    let trigger_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'trigger' AND name = 'issue_decisions_no_update'",
            [],
            |row| row.get(0),
        )
        .expect("capture exact decision append-only guard");
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TRIGGER issue_decisions_no_update;",
        )
        .expect("open test-only predecessor tamper window");
    connection
        .execute(
            "UPDATE issue_decisions
             SET predecessor_decision_id = 'tampered-missing-predecessor'
             WHERE decision_id = ?1",
            [&second.data.decision_id],
        )
        .expect("tamper predecessor chain");
    connection
        .execute_batch(&trigger_sql)
        .expect("restore exact decision append-only guard");
    drop(connection);
    assert_integrity_tamper_blocked(&fixture, &first.data.decision_id, &blocked_request);
}

#[test]
fn rejected_requests_create_no_false_authority_or_persistence() {
    let fixture = prepare_fixture();
    let service = decision_service(&fixture);
    let scope = issue_scope(&fixture, "SYN-PATTERN-001", 2);
    let baseline = inherited_snapshot(&fixture);

    let mut wrong_issue_hash = request(
        &fixture,
        scope.clone(),
        "invalid-issue-hash-1",
        IssueDisposition::Waived,
        None,
    );
    wrong_issue_hash.scope.issue_hash = "0".repeat(64);
    expect_error(service.apply(&wrong_issue_hash), "DECISION_CONFLICT");

    let mut wrong_profile = request(
        &fixture,
        scope.clone(),
        "invalid-profile-hash-1",
        IssueDisposition::Waived,
        None,
    );
    wrong_profile.scope.profile_hash = "1".repeat(64);
    expect_error(service.apply(&wrong_profile), "DECISION_CONFLICT");

    let mut wrong_mapping = request(
        &fixture,
        scope.clone(),
        "invalid-mapping-hash-1",
        IssueDisposition::Waived,
        None,
    );
    wrong_mapping.scope.mapping_hash = "2".repeat(64);
    expect_error(service.apply(&wrong_mapping), "DECISION_CONFLICT");

    let mut wrong_run = request(
        &fixture,
        scope.clone(),
        "invalid-run-identity-1",
        IssueDisposition::Waived,
        None,
    );
    wrong_run.scope.validation_run_id = "missing-validation-run".to_owned();
    expect_error(service.apply(&wrong_run), "DECISION_INTEGRITY_FAILED");

    let mut wrong_field = request(
        &fixture,
        scope.clone(),
        "invalid-field-identity-1",
        IssueDisposition::Corrected,
        Some(replacement("synthetic")),
    );
    wrong_field.scope.canonical_field_id = "category".to_owned();
    expect_error(service.apply(&wrong_field), "DECISION_INVALID");

    let mut zero_ordinal = request(
        &fixture,
        scope.clone(),
        "invalid-zero-source-ordinal-1",
        IssueDisposition::Waived,
        None,
    );
    zero_ordinal.scope.source_ordinal = 0;
    expect_error(zero_ordinal.validate(), "DECISION_INVALID");
    expect_error(service.apply(&zero_ordinal), "DECISION_INVALID");

    let mut leading_punctuation_key = request(
        &fixture,
        scope.clone(),
        "valid-before-leading-punctuation",
        IssueDisposition::Waived,
        None,
    );
    leading_punctuation_key.idempotency_key = "-leading-punctuation".to_owned();
    expect_error(leading_punctuation_key.validate(), "DECISION_INVALID");
    expect_error(service.apply(&leading_punctuation_key), "DECISION_INVALID");

    let mut leading_punctuation_identity = request(
        &fixture,
        scope.clone(),
        "invalid-leading-identity-1",
        IssueDisposition::Waived,
        None,
    );
    leading_punctuation_identity.scope.job_id = ".leading-punctuation".to_owned();
    expect_error(leading_punctuation_identity.validate(), "DECISION_INVALID");
    expect_error(
        service.apply(&leading_punctuation_identity),
        "DECISION_INVALID",
    );

    let ineligible = request(
        &fixture,
        issue_scope(&fixture, "SYN-UNIQUE-001", 3),
        "invalid-required-empty-1",
        IssueDisposition::Corrected,
        Some(CorrectionOverlayRequest {
            operation: CorrectionOperation::SetEffectiveEmpty,
            replacement_text: None,
        }),
    );
    expect_error(service.apply(&ineligible), "DECISION_INVALID");

    let too_long = request(
        &fixture,
        scope,
        "invalid-length-1",
        IssueDisposition::Corrected,
        Some(replacement(&"x".repeat(65))),
    );
    expect_error(service.apply(&too_long), "DECISION_INVALID");

    let connection = open_database(&fixture);
    assert_eq!(table_count(&connection, "decision_attempts"), 0);
    assert_eq!(table_count(&connection, "correction_overlays"), 0);
    assert_eq!(table_count(&connection, "reevaluation_runs"), 0);
    assert_eq!(table_count(&connection, "issue_decisions"), 0);
    assert_eq!(table_count(&connection, "correction_effectivations"), 0);
    let source_reference: Option<String> = connection
        .query_row(
            "SELECT accepted_source_file FROM decision_attempts LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .expect("read absent rejected attempt");
    assert!(source_reference.is_none());
    drop(connection);
    assert_eq!(inherited_snapshot(&fixture), baseline);
}

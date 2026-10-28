#![allow(clippy::too_many_lines)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use migration_stream::decision_read_model::DecisionReadService;
use migration_stream::decisions::{
    DECISION_SCHEMA_VERSION, DecisionConcurrency, DecisionScope, IssueDecisionRequest,
    IssueDisposition,
};
use migration_stream::intake::ingest_package;
use migration_stream::mapping::validate_mapping;
use migration_stream::policy::IntakePolicy;
use migration_stream::profile_registry::{RegistryLimits, approve_profile, register_profile};
use migration_stream::publication::{
    MEMBER_APPROVAL_EVIDENCE, MEMBER_EFFECTIVE_ROWS_CSV, MEMBER_EFFECTIVE_ROWS_JSON,
    MEMBER_EXCLUSION_EVIDENCE, MEMBER_ISSUE_DECISION_EVIDENCE, MEMBER_MANIFEST, MEMBER_PROVENANCE,
    MEMBER_RECONCILIATION_EVIDENCE, PUBLICATION_COMMAND_SCHEMA, PUBLICATION_CONTRACT_VERSION,
    PublicationRecord, PublicationRequest,
};
use migration_stream::publication_read_model::{
    PUBLICATION_DETAIL_SCHEMA, PUBLICATION_ELIGIBILITY_SCHEMA, PUBLICATION_LIST_SCHEMA,
    PublicationReadService,
};
use migration_stream::publication_service::PublicationService;
use migration_stream::reconciliation_approval::{
    RECONCILIATION_SCHEMA_VERSION, ResultActionKind, ResultActionPayload,
};
use migration_stream::reconciliation_service::{
    CalculateReconciliationRequest, CreateCandidateRequest, ReconciliationApprovalService,
};
use migration_stream::state::SCHEMA_VERSION;
use migration_stream::validation::{ValidationPolicy, run_validation};
use migration_stream::workbench::{WorkbenchConfig, build_router};
use rusqlite::{Connection, OptionalExtension, params};
use tempfile::TempDir;
use tower::ServiceExt as _;

// SHA-256 of the empty byte string — the disallowed placeholder value.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const ZERO_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";

const PROFILE_ID: &str = "synthetic.customer.readiness";
const PROFILE_VERSION: &str = "1.0.0";
const MAPPING_ID: &str = "synthetic.customer.valid";
const MAPPING_VERSION: &str = "1.0.0";
const CALCULATE_SCHEMA: &str = "migration-stream/write/calculate-reconciliation/v1";
const CANDIDATE_SCHEMA: &str = "migration-stream/write/create-result-candidate/v1";
const BOUNDARY_VERSION: &str = "migration-stream-result-boundary/v1";

fn project_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn prepare_workspace() -> (TempDir, PathBuf) {
    let temporary =
        tempfile::tempdir_in(project_path("target")).expect("create temporary workspace");
    let workspace = temporary.path().join("wp007-workspace");
    fs::create_dir(&workspace).expect("create selected object workspace");
    migration_stream::object_workspace::establish_for_local_automation(
        &workspace,
        "Synthetic publication workspace",
        "Customer readiness package",
        "Automated publication proof",
    )
    .expect("initialize object workspace");
    (temporary, workspace)
}

struct EligibleFixture {
    _temporary: TempDir,
    workspace: PathBuf,
    job_id: String,
    validation_run_id: String,
    profile_hash: String,
    mapping_id: String,
    mapping_version: String,
    mapping_hash: String,
}

/// Build the complete fixture used by `wp006_reconciliation` tests, replicating
/// the same canonical path:
/// intake → profile → mapping → validation → (close blockers) → calculate → candidate → approve.
fn prepare_eligible_fixture() -> EligibleFixture {
    prepare_eligible_fixture_with_mapping_id(MAPPING_ID)
}

fn prepare_eligible_fixture_with_mapping_id(mapping_id: &str) -> EligibleFixture {
    let temporary =
        tempfile::tempdir_in(project_path("target")).expect("link-safe temporary directory");
    let workspace = temporary.path().join("wp007-eligible-workspace");
    fs::create_dir(&workspace).expect("create selected object workspace");
    migration_stream::object_workspace::establish_for_local_automation(
        &workspace,
        "Synthetic eligible publication workspace",
        "Customer readiness package",
        "Automated publication proof",
    )
    .expect("initialize object workspace");

    let intake = ingest_package(
        &project_path("fixtures/packages/valid.zip"),
        &workspace,
        &IntakePolicy {
            minimum_free_space_bytes: 0,
            transaction_batch_rows: 2,
            ..IntakePolicy::default()
        },
    )
    .expect("ingest bounded synthetic package");

    let registered = register_profile(
        &workspace,
        &project_path("schemas/migration-object-profile.schema.json"),
        &project_path("fixtures/wp003/synthetic-profile.reviewed.json"),
        &RegistryLimits::default(),
    )
    .expect("register reviewed synthetic profile");
    let approved = approve_profile(
        &workspace,
        PROFILE_ID,
        PROFILE_VERSION,
        &registered.profile_hash,
        "WP-007 synthetic publication integration",
        "Authorize synthetic profile for WP-007 publication proof",
        true,
        &RegistryLimits::default(),
    )
    .expect("approve exact synthetic profile");

    let mapping_path = temporary.path().join("mapping.json");
    let mut mapping: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(project_path("fixtures/wp003/valid-mapping.template.json"))
            .expect("read synthetic mapping template"),
    )
    .expect("parse synthetic mapping template");
    mapping["mappingId"] = serde_json::Value::String(mapping_id.to_owned());
    mapping["profile"]["sha256"] = serde_json::Value::String(approved.profile_hash.clone());
    fs::write(
        &mapping_path,
        serde_json::to_vec_pretty(&mapping).expect("serialize finalized synthetic mapping"),
    )
    .expect("write finalized synthetic mapping");
    let validated_mapping = validate_mapping(
        &workspace,
        &intake.job_id,
        &project_path("schemas/source-mapping-version.schema.json"),
        &project_path("schemas/migration-object-profile.schema.json"),
        &mapping_path,
    )
    .expect("validate exact synthetic mapping");

    let validation = run_validation(
        &workspace,
        &intake.job_id,
        mapping_id,
        MAPPING_VERSION,
        &project_path("schemas/migration-object-profile.schema.json"),
        &ValidationPolicy::from_path(&project_path("fixtures/wp003/validation-policy.json"))
            .expect("load bounded validation policy"),
    )
    .expect("create immutable source validation evidence");
    assert_eq!(
        validation.result.issue_count, 5,
        "expected 5 synthetic issues"
    );

    // Close the 2 blocking issues (SYN-UNIQUE-001 ordinals 3 and 4) so the next
    // reconciliation snapshot has blocked_count == 0.
    close_eligible_blockers(&workspace, &intake.job_id, &validation.run_id);
    EligibleFixture {
        _temporary: temporary,
        workspace,
        job_id: intake.job_id,
        validation_run_id: validation.run_id,
        profile_hash: approved.profile_hash,
        mapping_id: mapping_id.to_owned(),
        mapping_version: MAPPING_VERSION.to_owned(),
        mapping_hash: validated_mapping.mapping_hash,
    }
}

fn prepare_approved_publication_fixture() -> EligibleFixture {
    prepare_approved_publication_fixture_with_mapping_id(MAPPING_ID)
}

fn prepare_approved_publication_fixture_with_mapping_id(mapping_id: &str) -> EligibleFixture {
    let fixture = prepare_eligible_fixture_with_mapping_id(mapping_id);
    let service = ReconciliationApprovalService::new(&fixture.workspace);

    let snapshot = service
        .calculate(&CalculateReconciliationRequest {
            schema_version: CALCULATE_SCHEMA.to_owned(),
            job_id: fixture.job_id.clone(),
            validation_run_id: fixture.validation_run_id.clone(),
            practitioner_id: "wp009-synthetic-reconciler".to_owned(),
            reason: "WP-009 bounded synthetic reconciliation for publication proof".to_owned(),
            idempotency_key: "wp009-calculate-publication-0001".to_owned(),
            local_readiness_only_acknowledged: true,
        })
        .expect("calculate reconciliation for WP-009 proof");
    assert_eq!(snapshot.blocked_count, 0);

    let candidate = service
        .create_candidate(&CreateCandidateRequest {
            schema_version: CANDIDATE_SCHEMA.to_owned(),
            job_id: fixture.job_id.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            snapshot_hash: snapshot.snapshot_hash.clone(),
            practitioner_id: "wp009-synthetic-preparer".to_owned(),
            reason: "Prepare bounded synthetic candidate for WP-009 publication proof".to_owned(),
            idempotency_key: "wp009-candidate-publication-0001".to_owned(),
            local_readiness_only_acknowledged: true,
        })
        .expect("create eligible result candidate for WP-009 proof");
    assert!(candidate.eligible);

    service
        .apply_action(
            &fixture.job_id,
            &ResultActionPayload {
                schema_version: RECONCILIATION_SCHEMA_VERSION.to_owned(),
                kind: ResultActionKind::Approved,
                candidate_id: candidate.candidate_id,
                candidate_hash: candidate.candidate_hash,
                snapshot_id: snapshot.snapshot_id,
                snapshot_hash: snapshot.snapshot_hash,
                actor: "wp009-synthetic-approver".to_owned(),
                reason: "Approve bounded synthetic candidate for WP-009 publication proof"
                    .to_owned(),
                warning_acknowledgements: vec!["NON_BLOCKING_VALIDATION_WARNING".to_owned()],
                authority_boundary_acknowledged: true,
                authority_boundary_acknowledgement_version: BOUNDARY_VERSION.to_owned(),
                predecessor_action_id: None,
                predecessor_action_hash: None,
                observed_generation: 0,
                idempotency_key: "wp009-approve-publication-0001".to_owned(),
            },
        )
        .expect("approve eligible candidate for WP-009 proof");

    fixture
}

fn publish_approved_fixture(
    fixture: &EligibleFixture,
    practitioner: &str,
    idempotency_key: &str,
) -> PublicationRecord {
    let eligibility = PublicationReadService::new(&fixture.workspace)
        .get_eligibility(&fixture.job_id)
        .expect("read approved publication eligibility");
    assert!(eligibility.is_eligible);
    PublicationService::new(&fixture.workspace)
        .publish(&PublicationRequest {
            schema_version: PUBLICATION_COMMAND_SCHEMA.to_owned(),
            job_id: fixture.job_id.clone(),
            candidate_id: eligibility
                .candidate_id
                .expect("approved fixture candidate identity"),
            approval_id: eligibility
                .approval_id
                .expect("approved fixture approval identity"),
            practitioner: practitioner.to_owned(),
            publication_reason: "Prove dynamic governing package provenance".to_owned(),
            acknowledgments: eligibility.required_acknowledgments,
            idempotency_key: idempotency_key.to_owned(),
        })
        .expect("publish approved fixture")
}

#[test]
fn exact_publication_request_is_durable_retry_identity() {
    use sha2::{Digest as _, Sha256};

    let fixture = prepare_approved_publication_fixture();
    let eligibility = PublicationReadService::new(&fixture.workspace)
        .get_eligibility(&fixture.job_id)
        .expect("read exact retry eligibility");
    let request = PublicationRequest {
        schema_version: PUBLICATION_COMMAND_SCHEMA.to_owned(),
        job_id: fixture.job_id.clone(),
        candidate_id: eligibility
            .candidate_id
            .expect("exact retry candidate identity"),
        approval_id: eligibility
            .approval_id
            .expect("exact retry approval identity"),
        practitioner: "wp009-retry-safe-publisher".to_owned(),
        publication_reason: "Prove one exact package under an unchanged retry".to_owned(),
        acknowledgments: eligibility.required_acknowledgments,
        idempotency_key: "wp009-exact-publication-retry-0001".to_owned(),
    };
    let service = PublicationService::new(&fixture.workspace);
    let first = service
        .publish(&request)
        .expect("first exact publication command");
    let replay = service
        .publish(&request)
        .expect("unchanged publication retry");
    assert_eq!(first, replay, "an unchanged retry returns the exact record");

    let mut changed = request.clone();
    changed.publication_reason = "A materially different command".to_owned();
    let error = service
        .publish(&changed)
        .expect_err("same idempotency key must reject a changed command");
    assert!(matches!(
        error,
        migration_stream::error::KernelError::Conflict(_)
    ));

    let canonical_request_json = migration_stream::canonical::canonical_json_string(
        &serde_json::to_value(&request).expect("publication request JSON"),
    )
    .expect("canonical exact publication request");
    let expected_request_hash = {
        let mut hasher = Sha256::new();
        hasher.update(canonical_request_json.as_bytes());
        migration_stream::canonical::lower_hex(hasher.finalize())
    };
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open retry database");
    let persisted: (i64, String, String) = connection
        .query_row(
            "SELECT COUNT(*), MIN(request_hash), MIN(canonical_request_json)
             FROM publication_attempts
             WHERE job_id = ?1 AND idempotency_key = ?2",
            params![fixture.job_id, request.idempotency_key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read durable publication request identity");
    assert_eq!(persisted.0, 1, "one request identity creates one package");
    assert_eq!(persisted.1, expected_request_hash);
    assert_eq!(persisted.2, canonical_request_json);
    assert_ne!(persisted.2, "{}");
}

fn read_archive_members(zip_bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).expect("open generated ZIP");
    let mut members = BTreeMap::new();
    for index in 0..archive.len() {
        let mut member = archive.by_index(index).expect("open generated ZIP member");
        let mut bytes = Vec::new();
        member
            .read_to_end(&mut bytes)
            .expect("read generated ZIP member");
        assert!(
            members
                .insert(
                    member
                        .name()
                        .expect("valid archive member name")
                        .into_owned(),
                    bytes
                )
                .is_none(),
            "archive member names must be unique"
        );
    }
    members
}

fn parse_json_member(members: &BTreeMap<String, Vec<u8>>, filename: &str) -> serde_json::Value {
    serde_json::from_slice(
        members
            .get(filename)
            .unwrap_or_else(|| panic!("missing {filename}")),
    )
    .unwrap_or_else(|error| panic!("parse {filename}: {error}"))
}

fn assert_safe_governing_identifier(value: &str) {
    assert_ne!(value, "");
    assert!(value.len() <= 256);
    assert!(value.as_bytes()[0].is_ascii_alphanumeric());
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    );
}

fn assert_non_placeholder_sha256(value: &str) {
    assert_eq!(value.len(), 64);
    assert!(
        value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    assert_ne!(value, ZERO_SHA256);
    assert_ne!(value, EMPTY_SHA256);
}

type ScopeRowTuple = (
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
);

fn issue_scope_from_db(
    workspace: &Path,
    job_id: &str,
    run_id: &str,
    rule_id: &str,
    ordinal: u64,
) -> DecisionScope {
    let db = Connection::open(workspace.join("state.sqlite3")).expect("open db for scope");
    let row: ScopeRowTuple = db
        .query_row(
            "SELECT vi.issue_id, vi.issue_hash, vi.source_file,
                    vi.line_start, vi.line_end, vi.entity_id, vi.field_id,
                    vr.profile_id, vr.profile_version, vr.profile_hash,
                    vr.mapping_id, vr.mapping_version, vr.mapping_hash
             FROM validation_issues vi
             JOIN validation_runs vr ON vr.id = vi.run_id
             WHERE vi.run_id = ?1 AND vi.rule_id = ?2 AND vi.source_ordinal = ?3",
            params![run_id, rule_id, i64::try_from(ordinal).unwrap()],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                    r.get(9)?,
                    r.get(10)?,
                    r.get(11)?,
                    r.get(12)?,
                ))
            },
        )
        .expect("select synthetic validation issue scope");
    DecisionScope {
        job_id: job_id.to_owned(),
        validation_run_id: run_id.to_owned(),
        issue_id: row.0,
        issue_hash: row.1,
        accepted_source_file: row.2,
        source_ordinal: ordinal,
        line_start: u64::try_from(row.3).expect("positive line_start"),
        line_end: u64::try_from(row.4).expect("positive line_end"),
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

fn close_eligible_blockers(workspace: &Path, job_id: &str, run_id: &str) {
    let db = Connection::open(workspace.join("state.sqlite3")).expect("open db for concurrency");

    let decision_service = migration_stream::decision_service::DecisionService::new(
        workspace,
        project_path("schemas/migration-object-profile.schema.json"),
    );

    // Close the two blocking SYN-UNIQUE-001 duplicate issues at ordinals 3 and 4.
    for (ord, disposition, key) in [
        (
            3u64,
            IssueDisposition::AcceptedAsIs,
            "wp007-close-row-three-0001",
        ),
        (
            4u64,
            IssueDisposition::Excluded,
            "wp007-exclude-row-four-0001",
        ),
    ] {
        let scope = issue_scope_from_db(workspace, job_id, run_id, "SYN-UNIQUE-001", ord);

        // Read current concurrency state (expected_active_decision_id may be None on first write)
        let existing: Option<(String, i64)> = db
            .query_row(
                "SELECT decision_id, generation FROM issue_decisions
                 WHERE job_id = ? AND issue_id = ?
                 ORDER BY generation DESC LIMIT 1",
                params![job_id, &scope.issue_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .expect("query existing decision");

        let detail = DecisionReadService::new(workspace)
            .issue_detail(job_id, run_id, &scope.issue_id)
            .expect("read issue detail for concurrency");

        let expected_effective_value_hash = (disposition == IssueDisposition::AcceptedAsIs)
            .then_some(detail.data.concurrency.expected_effective_value_hash);

        let concurrency = match &existing {
            None => DecisionConcurrency {
                expected_active_decision_id: None,
                expected_active_decision_generation: 0,
                expected_effective_value_hash,
            },
            Some((id, generation)) => DecisionConcurrency {
                expected_active_decision_id: Some(id.clone()),
                expected_active_decision_generation: u64::try_from(*generation).unwrap(),
                expected_effective_value_hash,
            },
        };

        let request = IssueDecisionRequest {
            schema_version: DECISION_SCHEMA_VERSION,
            idempotency_key: key.to_owned(),
            disposition,
            scope,
            concurrency,
            practitioner_id: "wp007-synthetic-decider".to_owned(),
            reason: "Close blocking issue for WP-007 synthetic publication proof".to_owned(),
            evidence_reference: Some("WP007-SYNTHETIC-EVIDENCE-ONLY".to_owned()),
            authority_separation_acknowledged: true,
            row_scope_exclusion_acknowledged: disposition == IssueDisposition::Excluded,
            correction_overlay: None,
        };
        decision_service
            .apply(&request)
            .expect("close blocking issue for WP-007 proof");
    }
}

#[test]
fn current_schema_preserves_publication_tables() {
    let (_temp, workspace) = prepare_workspace();
    let db_path = workspace.join("state.sqlite3");
    let conn = Connection::open(&db_path).expect("open database");

    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .expect("get user_version");
    assert_eq!(
        version, SCHEMA_VERSION,
        "workspace must use the current schema"
    );

    // Check table existence
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();

    assert!(tables.contains(&"publication_attempts".to_owned()));
    assert!(tables.contains(&"publication_records".to_owned()));
    assert!(tables.contains(&"publication_manifest_members".to_owned()));
}

#[test]
fn test_publication_read_model_empty() {
    let (_temp, workspace) = prepare_workspace();
    let read_service = PublicationReadService::new(&workspace);

    let list = read_service
        .list_publications("job_123")
        .expect("list publications");
    assert_eq!(list.total_count, 0);
    assert_eq!(
        list.items,
        [] as [migration_stream::publication_read_model::PublicationSummaryItem; 0]
    );

    let detail = read_service
        .get_detail("pub_nonexistent")
        .expect("get detail");
    assert!(detail.is_none());

    let eligibility = read_service
        .get_eligibility("job_123")
        .expect("get eligibility");
    assert!(!eligibility.is_eligible);
    assert_ne!(eligibility.blockers, [] as [std::string::String; 0]);
}

#[test]
fn publication_detail_read_is_scoped_to_the_selected_job() {
    let fixture = prepare_approved_publication_fixture();
    let record = publish_approved_fixture(
        &fixture,
        "wp009-scoped-detail-reader",
        "wp009-scoped-detail-publication-0001",
    );
    let reads = PublicationReadService::new(&fixture.workspace);

    let selected = reads
        .get_detail_for_job(&fixture.job_id, &record.publication_id)
        .expect("read publication for its selected job");
    assert_eq!(
        selected
            .as_ref()
            .map(|publication| publication.publication_id.as_str()),
        Some(record.publication_id.as_str())
    );

    let outside_scope = reads
        .get_detail_for_job("job-outside-startup-scope", &record.publication_id)
        .expect("scope mismatch remains a successful empty read");
    assert!(outside_scope.is_none());
}

#[tokio::test]
async fn workbench_publication_detail_and_download_hide_other_jobs() {
    let fixture = prepare_eligible_fixture();
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open scoped workspace");
    connection
        .execute_batch(
            "INSERT INTO jobs (
                 id, state, source_name, source_path, policy_json, created_at, updated_at
             ) VALUES (
                 'job-outside-startup-scope', 'INSPECTING', 'foreign.zip', '/foreign.zip',
                 '{}', '2026-07-28T00:00:00Z', '2026-07-28T00:00:00Z'
             );
             INSERT INTO publication_attempts (
                 attempt_id, schema_version, job_id, candidate_id, candidate_hash,
                 approval_id, approval_hash, contract_version, practitioner,
                 publication_reason, idempotency_key, request_hash,
                 canonical_request_json, created_at
             ) VALUES (
                 'publication-other-job', 1, 'job-outside-startup-scope', 'candidate-other-job',
                 '1111111111111111111111111111111111111111111111111111111111111111',
                 'approval-other-job',
                 '2222222222222222222222222222222222222222222222222222222222222222',
                 'migration-stream/readiness-package/v1', 'foreign-practitioner',
                 'Foreign job publication evidence', 'foreign-job-publication-0001',
                 '3333333333333333333333333333333333333333333333333333333333333333',
                 '{}', '2026-07-28T00:00:00Z'
             );
             INSERT INTO publication_records (
                 publication_id, schema_version, attempt_id, job_id, candidate_id,
                 candidate_hash, approval_id, approval_hash, contract_version,
                 semantic_publication_hash, physical_archive_digest, package_size_bytes,
                 member_count, included_row_count, excluded_row_count, archive_relative_path,
                 practitioner, publication_reason, canonical_manifest_json, created_at
             ) VALUES (
                 'publication-other-job', 1, 'publication-other-job',
                 'job-outside-startup-scope', 'candidate-other-job',
                 '1111111111111111111111111111111111111111111111111111111111111111',
                 'approval-other-job',
                 '2222222222222222222222222222222222222222222222222222222222222222',
                 'migration-stream/readiness-package/v1',
                 '4444444444444444444444444444444444444444444444444444444444444444',
                 '5555555555555555555555555555555555555555555555555555555555555555',
                 1, 0, 0, 0, 'publications/completed/foreign.zip',
                 'foreign-practitioner', 'Foreign job publication evidence', '{}',
                 '2026-07-28T00:00:00Z'
             );",
        )
        .expect("insert publication outside the startup-selected job");
    drop(connection);

    let app = build_router(
        WorkbenchConfig::new(&fixture.workspace, fixture.job_id, None).with_readiness_publication(),
    )
    .expect("preflight selected-job publication workbench");

    for uri in [
        "/api/v1/publications/publication-other-job",
        "/api/v1/publications/publication-other-job/download",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("host", "127.0.0.1:4173")
                    .body(Body::empty())
                    .expect("bounded cross-job publication request"),
            )
            .await
            .expect("infallible cross-job publication response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn test_workbench_publication_eligibility_response_identifies_contract() {
    let fixture = prepare_eligible_fixture();
    let app = build_router(
        WorkbenchConfig::new(&fixture.workspace, fixture.job_id, None).with_readiness_publication(),
    )
    .expect("preflight synthetic publication workbench");
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/publications/eligibility")
                .header("host", "127.0.0.1:4173")
                .body(Body::empty())
                .expect("bounded publication eligibility request"),
        )
        .await
        .expect("infallible workbench response");

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 16 * 1024)
        .await
        .expect("bounded publication eligibility response");
    let document: serde_json::Value =
        serde_json::from_slice(&body).expect("publication eligibility JSON");
    assert_eq!(document["schema_version"], PUBLICATION_ELIGIBILITY_SCHEMA);
    assert_eq!(document["is_eligible"], false);
}

#[tokio::test]
async fn test_workbench_v3_and_v4_publication_capability_contracts() {
    let fixture = prepare_eligible_fixture();
    let v3 = build_router(
        WorkbenchConfig::new(&fixture.workspace, fixture.job_id.clone(), None)
            .with_migration_result_approval(),
    )
    .expect("preflight synthetic v3 workbench");
    let v3_publication_route = v3.clone();
    let response = v3
        .oneshot(
            Request::builder()
                .uri("/api/v1/context")
                .header("host", "127.0.0.1:4173")
                .body(Body::empty())
                .expect("bounded v3 context request"),
        )
        .await
        .expect("infallible v3 context response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 32 * 1024)
        .await
        .expect("bounded v3 context response");
    let v3_document: serde_json::Value = serde_json::from_slice(&body).expect("v3 context JSON");
    assert_eq!(
        v3_document["schema_version"],
        "migration-stream/workbench/context/v3"
    );
    assert_eq!(v3_document["capabilities"]["publication"], false);
    let v3_contracts = v3_document["supported_contracts"]
        .as_array()
        .expect("v3 supported contracts");
    for publication_contract in [
        PUBLICATION_ELIGIBILITY_SCHEMA,
        PUBLICATION_LIST_SCHEMA,
        PUBLICATION_DETAIL_SCHEMA,
        PUBLICATION_COMMAND_SCHEMA,
        PUBLICATION_CONTRACT_VERSION,
    ] {
        assert!(
            !v3_contracts
                .iter()
                .any(|value| value == publication_contract),
            "v3 must not advertise {publication_contract}"
        );
    }
    let disabled_response = v3_publication_route
        .oneshot(
            Request::builder()
                .uri("/api/v1/publications/eligibility")
                .header("host", "127.0.0.1:4173")
                .body(Body::empty())
                .expect("bounded v3 publication request"),
        )
        .await
        .expect("infallible v3 publication response");
    assert_eq!(disabled_response.status(), StatusCode::FORBIDDEN);

    let v4 = build_router(
        WorkbenchConfig::new(&fixture.workspace, fixture.job_id, None).with_readiness_publication(),
    )
    .expect("preflight synthetic v4 workbench");
    let response = v4
        .oneshot(
            Request::builder()
                .uri("/api/v1/context")
                .header("host", "127.0.0.1:4173")
                .body(Body::empty())
                .expect("bounded v4 context request"),
        )
        .await
        .expect("infallible v4 context response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 32 * 1024)
        .await
        .expect("bounded v4 context response");
    let v4_document: serde_json::Value = serde_json::from_slice(&body).expect("v4 context JSON");
    assert_eq!(
        v4_document["schema_version"],
        "migration-stream/workbench/context/v4"
    );
    assert_eq!(v4_document["capabilities"]["publication"], true);
    assert_eq!(v4_document["capabilities"]["target_write"], false);
    let v4_contracts = v4_document["supported_contracts"]
        .as_array()
        .expect("v4 supported contracts");
    for publication_contract in [
        "migration-stream/workbench/context/v4",
        PUBLICATION_ELIGIBILITY_SCHEMA,
        PUBLICATION_LIST_SCHEMA,
        PUBLICATION_DETAIL_SCHEMA,
        PUBLICATION_COMMAND_SCHEMA,
        PUBLICATION_CONTRACT_VERSION,
    ] {
        assert!(
            v4_contracts
                .iter()
                .any(|value| value == publication_contract),
            "v4 must advertise {publication_contract}"
        );
    }
    assert!(
        !v4_contracts.iter().any(|value| {
            value.as_str().is_some_and(|contract| {
                contract.contains("external") || contract.contains("target")
            })
        }),
        "v4 must not advertise external target contracts"
    );
}

#[tokio::test]
async fn test_publication_command_detail_and_list_preserve_distinct_contracts() {
    let fixture = prepare_approved_publication_fixture();
    let app = build_router(
        WorkbenchConfig::new(&fixture.workspace, fixture.job_id.clone(), None)
            .with_readiness_publication(),
    )
    .expect("preflight approved synthetic publication workbench");

    let eligibility = PublicationReadService::new(&fixture.workspace)
        .get_eligibility(&fixture.job_id)
        .expect("read approved publication eligibility");
    assert!(eligibility.is_eligible);

    let command = serde_json::json!({
        "schema_version": PUBLICATION_COMMAND_SCHEMA,
        "job_id": fixture.job_id,
        "candidate_id": eligibility.candidate_id,
        "approval_id": eligibility.approval_id,
        "practitioner": "wp009-http-publisher",
        "publication_reason": "Prove versioned WP-009 Publication HTTP contracts",
        "acknowledgments": eligibility.required_acknowledgments,
        "idempotency_key": "wp009-http-publication-0001"
    });
    let mut outside_startup_scope = command.clone();
    outside_startup_scope["job_id"] =
        serde_json::Value::String("job-outside-startup-scope".to_owned());
    let outside_scope_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/publications")
                .header("host", "127.0.0.1:4173")
                .header("origin", "http://127.0.0.1:4173")
                .header("content-type", "application/json")
                .header("idempotency-key", "wp009-http-publication-0001")
                .header("x-migration-stream-intent", "readiness-publication-v1")
                .body(Body::from(outside_startup_scope.to_string()))
                .expect("bounded outside-scope publication command"),
        )
        .await
        .expect("infallible outside-scope publication response");
    assert_eq!(outside_scope_response.status(), StatusCode::BAD_REQUEST);

    let missing_intent_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/publications")
                .header("host", "127.0.0.1:4173")
                .header("origin", "http://127.0.0.1:4173")
                .header("content-type", "application/json")
                .header("idempotency-key", "wp009-http-publication-0001")
                .body(Body::from(command.to_string()))
                .expect("bounded missing-intent publication command"),
        )
        .await
        .expect("infallible missing-intent publication response");
    assert_eq!(missing_intent_response.status(), StatusCode::BAD_REQUEST);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/publications")
                .header("host", "127.0.0.1:4173")
                .header("origin", "http://127.0.0.1:4173")
                .header("content-type", "application/json")
                .header("idempotency-key", "wp009-http-publication-0001")
                .header("x-migration-stream-intent", "readiness-publication-v1")
                .body(Body::from(command.to_string()))
                .expect("bounded publication command request"),
        )
        .await
        .expect("infallible publication command response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 256 * 1024)
        .await
        .expect("bounded publication command response body");
    let result: serde_json::Value =
        serde_json::from_slice(&body).expect("publication command result JSON");
    assert_eq!(result["schema_version"], PUBLICATION_DETAIL_SCHEMA);
    let publication_id = result["publication_id"]
        .as_str()
        .expect("publication identity")
        .to_owned();

    let detail_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/publications/{publication_id}"))
                .header("host", "127.0.0.1:4173")
                .body(Body::empty())
                .expect("bounded publication detail request"),
        )
        .await
        .expect("infallible publication detail response");
    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail_body = to_bytes(detail_response.into_body(), 256 * 1024)
        .await
        .expect("bounded publication detail body");
    let detail: serde_json::Value =
        serde_json::from_slice(&detail_body).expect("publication detail JSON");
    assert_eq!(detail["schema_version"], PUBLICATION_DETAIL_SCHEMA);
    assert_eq!(detail["publication_id"], publication_id);

    let list_response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/publications")
                .header("host", "127.0.0.1:4173")
                .body(Body::empty())
                .expect("bounded publication list request"),
        )
        .await
        .expect("infallible publication list response");
    assert_eq!(list_response.status(), StatusCode::OK);
    let list_body = to_bytes(list_response.into_body(), 256 * 1024)
        .await
        .expect("bounded publication list body");
    let list: serde_json::Value =
        serde_json::from_slice(&list_body).expect("publication list JSON");
    assert_eq!(list["schema_version"], PUBLICATION_LIST_SCHEMA);
    assert_eq!(list["total_count"], 1);
    assert_eq!(list["items"][0]["publication_id"], publication_id);
}

#[test]
fn test_publication_attempt_immutable_triggers() {
    let (_temp, workspace) = prepare_workspace();
    let db_path = workspace.join("state.sqlite3");
    let conn = Connection::open(&db_path).expect("open database");

    conn.execute(
        "INSERT INTO jobs (id, state, source_name, source_path, policy_json, created_at, updated_at)
         VALUES ('job_test', 'INSPECTING', 'src.zip', '/path', '{}', '2026-07-23T00:00:00Z', '2026-07-23T00:00:00Z')",
        [],
    ).unwrap();

    conn.execute(
        "INSERT INTO publication_attempts (
            attempt_id, schema_version, job_id, candidate_id, candidate_hash,
            approval_id, approval_hash, contract_version, practitioner, publication_reason,
            idempotency_key, request_hash, canonical_request_json, created_at
         ) VALUES (
            'pub_1', 1, 'job_test', 'cand_1', '0000000000000000000000000000000000000000000000000000000000000000',
            'appr_1', '0000000000000000000000000000000000000000000000000000000000000000',
            'migration-stream/readiness-package/v1', 'practitioner_1', 'reason', 'idem_1',
            '0000000000000000000000000000000000000000000000000000000000000000', '{}', '2026-07-23T00:00:00Z'
         )",
        [],
    ).unwrap();

    // Test UPDATE trigger rejection
    let update_res = conn.execute(
        "UPDATE publication_attempts SET practitioner = 'hacker' WHERE attempt_id = 'pub_1'",
        [],
    );
    assert!(
        update_res.is_err(),
        "UPDATE on publication_attempts must fail"
    );

    // Test DELETE trigger rejection
    let delete_res = conn.execute(
        "DELETE FROM publication_attempts WHERE attempt_id = 'pub_1'",
        [],
    );
    assert!(
        delete_res.is_err(),
        "DELETE on publication_attempts must fail"
    );
}

#[test]
fn test_publication_provenance_uses_dynamic_governing_identities() {
    let dynamic_mapping_id = format!(
        "synthetic.customer.publication.{}",
        uuid::Uuid::new_v4().simple()
    );
    let fixture = prepare_approved_publication_fixture_with_mapping_id(&dynamic_mapping_id);
    let record = publish_approved_fixture(
        &fixture,
        "wp009-dynamic-provenance-publisher",
        "wp009-dynamic-provenance-publication-0001",
    );
    let zip_bytes =
        fs::read(fixture.workspace.join(&record.archive_relative_path)).expect("read dynamic ZIP");
    let members = read_archive_members(&zip_bytes);
    let provenance = parse_json_member(&members, MEMBER_PROVENANCE);
    let reconciliation = parse_json_member(&members, MEMBER_RECONCILIATION_EVIDENCE);

    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open dynamic database");
    let authoritative: (String, String, String, String, String, String, String) = connection
        .query_row(
            "SELECT s.mapping_id, s.mapping_version, s.mapping_hash,
                    s.validation_run_id, s.validation_result_hash,
                    s.snapshot_id, s.snapshot_hash
             FROM migration_result_candidates c
             JOIN reconciliation_snapshots s
               ON s.snapshot_id = c.snapshot_id
              AND s.snapshot_hash = c.snapshot_hash
              AND s.job_id = c.job_id
             WHERE c.candidate_id = ?1",
            [&record.candidate_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .expect("read dynamic immutable governing provenance");

    assert_ne!(dynamic_mapping_id, MAPPING_ID);
    assert_eq!(fixture.mapping_id, dynamic_mapping_id);
    assert_eq!(authoritative.0, dynamic_mapping_id);
    assert_eq!(provenance["contract_version"], PUBLICATION_CONTRACT_VERSION);
    assert_eq!(
        provenance["schema_version"],
        migration_stream::state::SCHEMA_VERSION
    );
    assert_eq!(provenance["mapping_id"], authoritative.0);
    assert_eq!(provenance["mapping_version"], authoritative.1);
    assert_eq!(provenance["mapping_hash"], authoritative.2);
    assert_eq!(provenance["validation_run_id"], authoritative.3);
    assert_eq!(provenance["validation_result_hash"], authoritative.4);
    assert_eq!(
        reconciliation["reconciliation_snapshot_id"],
        authoritative.5
    );
    assert_eq!(
        reconciliation["reconciliation_snapshot_hash"],
        authoritative.6
    );
    assert_eq!(record.manifest.mapping_digest, authoritative.2);
    assert_eq!(record.manifest.validation_digest, authoritative.4);
    assert_eq!(
        record.manifest.reconciliation_snapshot_hash,
        authoritative.6
    );

    for identifier in [
        provenance["mapping_id"].as_str(),
        provenance["mapping_version"].as_str(),
        provenance["validation_run_id"].as_str(),
        reconciliation["reconciliation_snapshot_id"].as_str(),
    ] {
        assert_safe_governing_identifier(identifier.expect("governing identifier"));
    }
    for digest in [
        provenance["mapping_hash"].as_str(),
        provenance["validation_result_hash"].as_str(),
        reconciliation["reconciliation_snapshot_hash"].as_str(),
    ] {
        assert_non_placeholder_sha256(digest.expect("governing digest"));
    }
    println!(
        "dynamic_governing_provenance={}",
        serde_json::json!({
            "mapping_id": authoritative.0,
            "mapping_version": authoritative.1,
            "mapping_hash": authoritative.2,
            "validation_run_id": authoritative.3,
            "validation_result_hash": authoritative.4,
            "reconciliation_snapshot_id": authoritative.5,
            "reconciliation_snapshot_hash": authoritative.6,
        })
    );
}

/// Verifies that a real publication generates actual (non-placeholder) semantic and physical
/// digests, that both are well-formed 64-hex-character SHA-256 strings, that neither is the
/// empty-input placeholder, and that the physical digest matches the generated ZIP file on disk.
///
/// When `WP007_PROOF_OUTPUT_DIR` is set (by the smoke script), this test also writes a
/// `wp007-proof.json` file containing the real evidence values for the smoke summary.
#[test]
fn test_real_publication_generates_non_placeholder_digests() {
    use sha2::{Digest as _, Sha256};

    let fixture = prepare_eligible_fixture();

    // Drive the reconciliation → candidate → approval chain to reach eligible state.
    let service = ReconciliationApprovalService::new(&fixture.workspace);

    let snapshot = service
        .calculate(&CalculateReconciliationRequest {
            schema_version: CALCULATE_SCHEMA.to_owned(),
            job_id: fixture.job_id.clone(),
            validation_run_id: fixture.validation_run_id.clone(),
            practitioner_id: "wp007-synthetic-reconciler".to_owned(),
            reason: "WP-007 bounded synthetic reconciliation for publication proof".to_owned(),
            idempotency_key: "wp007-calculate-pub-proof-0001".to_owned(),
            local_readiness_only_acknowledged: true,
        })
        .expect("calculate reconciliation for WP-007 proof");
    assert_eq!(
        snapshot.blocked_count, 0,
        "snapshot must have zero blocked rows after closing fixture blockers"
    );

    let candidate = service
        .create_candidate(&CreateCandidateRequest {
            schema_version: CANDIDATE_SCHEMA.to_owned(),
            job_id: fixture.job_id.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            snapshot_hash: snapshot.snapshot_hash.clone(),
            practitioner_id: "wp007-synthetic-preparer".to_owned(),
            reason: "Prepare exact bounded synthetic candidate for WP-007 publication proof"
                .to_owned(),
            idempotency_key: "wp007-candidate-pub-proof-0001".to_owned(),
            local_readiness_only_acknowledged: true,
        })
        .expect("create eligible result candidate for WP-007 proof");
    assert!(
        candidate.eligible,
        "synthetic candidate must be eligible for publication proof"
    );

    service
        .apply_action(
            &fixture.job_id,
            &ResultActionPayload {
                schema_version: RECONCILIATION_SCHEMA_VERSION.to_owned(),
                kind: ResultActionKind::Approved,
                candidate_id: candidate.candidate_id.clone(),
                candidate_hash: candidate.candidate_hash.clone(),
                snapshot_id: snapshot.snapshot_id.clone(),
                snapshot_hash: snapshot.snapshot_hash.clone(),
                actor: "wp007-synthetic-approver".to_owned(),
                reason: "Approve bounded synthetic candidate for WP-007 publication proof"
                    .to_owned(),
                warning_acknowledgements: vec!["NON_BLOCKING_VALIDATION_WARNING".to_owned()],
                authority_boundary_acknowledged: true,
                authority_boundary_acknowledgement_version: BOUNDARY_VERSION.to_owned(),
                predecessor_action_id: None,
                predecessor_action_hash: None,
                observed_generation: 0,
                idempotency_key: "wp007-approve-pub-proof-0001".to_owned(),
            },
        )
        .expect("approve eligible candidate for WP-007 proof");

    // Confirm eligibility before publishing
    let read_service = PublicationReadService::new(&fixture.workspace);
    let eligibility = read_service
        .get_eligibility(&fixture.job_id)
        .expect("get publication eligibility");
    assert_eq!(eligibility.schema_version, PUBLICATION_ELIGIBILITY_SCHEMA);
    assert!(
        eligibility.is_eligible,
        "workspace must be eligible before publication: {:?}",
        eligibility.blockers
    );

    let candidate_id = eligibility
        .candidate_id
        .clone()
        .expect("eligible state must have candidate_id");
    let approval_id = eligibility
        .approval_id
        .clone()
        .expect("eligible state must have approval_id");

    // Execute a real publication and capture real digest values.
    let pub_service = PublicationService::new(&fixture.workspace);
    let record = pub_service
        .publish(&migration_stream::publication::PublicationRequest {
            schema_version: PUBLICATION_COMMAND_SCHEMA.to_owned(),
            job_id: fixture.job_id.clone(),
            candidate_id,
            approval_id,
            practitioner: "wp007-synthetic-publisher".to_owned(),
            publication_reason: "WP-007 bounded synthetic publication proof".to_owned(),
            acknowledgments: vec![
                "I confirm that this readiness package is local readiness evidence only."
                    .to_owned(),
                "I acknowledge that this package does not execute or authorize a target load."
                    .to_owned(),
            ],
            idempotency_key: "wp007-publish-real-digest-test-0001".to_owned(),
        })
        .expect("real publication must succeed on eligible workspace");

    let semantic = &record.semantic_publication_hash;
    let physical = &record.physical_archive_digest;

    // --- semantic_publication_hash: non-placeholder, well-formed SHA-256 ---
    assert_eq!(
        semantic.len(),
        64,
        "semantic_publication_hash must be 64 hex characters, got: {semantic}"
    );
    assert!(
        semantic.chars().all(|c| c.is_ascii_hexdigit()),
        "semantic_publication_hash must be lowercase hex, got: {semantic}"
    );
    assert_ne!(
        semantic, EMPTY_SHA256,
        "semantic_publication_hash must not be the empty-string SHA-256 placeholder"
    );

    // --- physical_archive_digest: non-placeholder, well-formed SHA-256 ---
    assert_eq!(
        physical.len(),
        64,
        "physical_archive_digest must be 64 hex characters, got: {physical}"
    );
    assert!(
        physical.chars().all(|c| c.is_ascii_hexdigit()),
        "physical_archive_digest must be lowercase hex, got: {physical}"
    );
    assert_ne!(
        physical, EMPTY_SHA256,
        "physical_archive_digest must not be the empty-string SHA-256 placeholder"
    );

    // --- physical digest must match the actual ZIP file on disk ---
    let zip_path = fixture.workspace.join(&record.archive_relative_path);
    assert!(
        zip_path.exists(),
        "generated ZIP must exist at: {zip_path:?}"
    );
    let zip_bytes = fs::read(&zip_path).expect("read generated ZIP file");
    let actual_physical = {
        let mut hasher = Sha256::new();
        hasher.update(&zip_bytes);
        migration_stream::canonical::lower_hex(hasher.finalize())
    };
    assert_eq!(
        physical, &actual_physical,
        "physical_archive_digest in record must match SHA-256 of the generated ZIP file"
    );

    // --- archive members and declared member evidence must be exact ---
    let members = read_archive_members(&zip_bytes);
    assert_eq!(
        members.keys().cloned().collect::<BTreeSet<_>>(),
        [
            MEMBER_APPROVAL_EVIDENCE,
            MEMBER_EFFECTIVE_ROWS_CSV,
            MEMBER_EFFECTIVE_ROWS_JSON,
            MEMBER_EXCLUSION_EVIDENCE,
            MEMBER_ISSUE_DECISION_EVIDENCE,
            MEMBER_MANIFEST,
            MEMBER_PROVENANCE,
            MEMBER_RECONCILIATION_EVIDENCE,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>(),
        "archive must contain exactly the allow-listed readiness-package members"
    );
    assert_eq!(members.len(), 8, "the ZIP must contain eight exact entries");
    assert_eq!(
        record.member_count, 7,
        "member_count is the non-recursive content inventory count"
    );
    assert_eq!(
        members.len(),
        record.member_count + 1,
        "manifest.json is the additional self-describing ZIP entry"
    );
    assert_eq!(record.manifest.member_inventory.len(), 7);
    for declared in &record.manifest.member_inventory {
        let bytes = members
            .get(&declared.filename)
            .unwrap_or_else(|| panic!("missing declared member {}", declared.filename));
        assert_eq!(declared.byte_size, bytes.len() as u64);
        let actual_digest = {
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            migration_stream::canonical::lower_hex(hasher.finalize())
        };
        assert_eq!(declared.sha256_digest, actual_digest);
    }

    // --- effective-row members must contain the exact declared included rows ---
    let effective_rows = parse_json_member(&members, MEMBER_EFFECTIVE_ROWS_JSON);
    let rows = effective_rows["rows"]
        .as_array()
        .expect("effective-row JSON array");
    assert_eq!(effective_rows["effective_row_count"], 3);
    assert_eq!(
        rows.len(),
        3,
        "declared effective rows must not be placeholders"
    );
    assert!(rows.iter().all(|row| {
        let values = row["canonical_values"].as_object();
        let lineage = row["lineage"].as_object();
        values.is_some_and(|values| {
            !values.is_empty()
                && lineage.is_some_and(|lineage| {
                    values.keys().collect::<BTreeSet<_>>()
                        == lineage.keys().collect::<BTreeSet<_>>()
                })
        })
    }));
    assert_eq!(
        rows.iter()
            .map(|row| {
                row["source_ordinal"]
                    .as_u64()
                    .expect("effective row source ordinal")
            })
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([1, 2, 3]),
        "the excluded fourth source row must be absent"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["category"] == "INCLUDED_VALID")
            .count(),
        2
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["category"] == "INCLUDED_ACCEPTED_AS_IS")
            .count(),
        1
    );

    let mut csv = csv::Reader::from_reader(
        members
            .get(MEMBER_EFFECTIVE_ROWS_CSV)
            .expect("effective-row CSV member")
            .as_slice(),
    );
    assert_eq!(
        csv.headers()
            .expect("effective-row CSV header")
            .iter()
            .collect::<Vec<_>>(),
        vec![
            "source_file",
            "source_ordinal",
            "line_start",
            "line_end",
            "entity_id",
            "category",
            "canonical_values_json",
        ]
    );
    assert_eq!(
        csv.records()
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("parse effective-row CSV")
            .len(),
        3,
        "effective-row CSV must contain every declared included row"
    );
    assert_eq!(record.manifest.included_valid_count, 2);
    assert_eq!(record.manifest.included_accepted_as_is_count, 1);
    assert_eq!(record.manifest.excluded_count, 1);
    assert_eq!(record.manifest.blocked_unresolved_count, 0);
    assert_eq!(record.manifest.blocked_incomplete_count, 0);
    assert_eq!(record.manifest.blocked_conflict_count, 0);
    assert_eq!(record.manifest.total_accepted_rows, 4);
    assert_eq!(record.manifest.governing_decision_count, Some(2));

    for filename in [
        MEMBER_MANIFEST,
        MEMBER_EFFECTIVE_ROWS_JSON,
        MEMBER_EXCLUSION_EVIDENCE,
        MEMBER_ISSUE_DECISION_EVIDENCE,
        MEMBER_PROVENANCE,
        MEMBER_RECONCILIATION_EVIDENCE,
        MEMBER_APPROVAL_EVIDENCE,
    ] {
        parse_json_member(&members, filename);
    }

    let mut semantic_hasher = Sha256::new();
    let mut declared_members = record.manifest.member_inventory.clone();
    declared_members.sort_by(|left, right| left.filename.cmp(&right.filename));
    for declared in &declared_members {
        semantic_hasher.update(declared.filename.as_bytes());
        semantic_hasher.update(
            members
                .get(&declared.filename)
                .expect("declared semantic member"),
        );
    }
    let member_sequence_digest = semantic_hasher.finalize();
    let semantic_publication_hash = {
        let mut hasher = Sha256::new();
        hasher.update(member_sequence_digest);
        migration_stream::canonical::lower_hex(hasher.finalize())
    };
    assert_eq!(
        record.semantic_publication_hash, semantic_publication_hash,
        "semantic publication hash must replay the existing complete-member contract"
    );

    // --- contract version matches canonical constant ---
    assert_eq!(
        record.schema_version, PUBLICATION_DETAIL_SCHEMA,
        "publication detail/result must carry its authoritative response contract"
    );
    assert_eq!(
        record.contract_version, PUBLICATION_CONTRACT_VERSION,
        "contract version must match canonical constant"
    );

    // --- manifest and record semantic hashes are consistent ---
    assert_eq!(
        record.semantic_publication_hash, record.manifest.semantic_publication_hash,
        "record.semantic_publication_hash must match manifest.semantic_publication_hash"
    );
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open proof database");
    let authoritative: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = connection
        .query_row(
            "SELECT accepted_source_hash, profile_id, profile_version, profile_hash,
                    mapping_id, mapping_version, mapping_hash,
                    validation_run_id, validation_result_hash,
                    snapshot_id, snapshot_hash
             FROM reconciliation_snapshots
             WHERE snapshot_id = ?1",
            [&snapshot.snapshot_id],
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
                ))
            },
        )
        .expect("read immutable snapshot provenance");
    assert_eq!(record.manifest.source_package_digest, authoritative.0);
    assert_eq!(record.manifest.profile_identity, authoritative.1);
    assert_eq!(record.manifest.profile_version, authoritative.2);
    assert_eq!(record.manifest.profile_digest, authoritative.3);
    assert_eq!(record.manifest.mapping_digest, authoritative.6);
    assert_eq!(record.manifest.validation_digest, authoritative.8);
    assert_eq!(
        record.manifest.reconciliation_snapshot_hash,
        authoritative.10
    );
    assert_eq!(authoritative.1, PROFILE_ID);
    assert_eq!(authoritative.2, PROFILE_VERSION);
    assert_eq!(authoritative.3, fixture.profile_hash);
    assert_eq!(authoritative.4, fixture.mapping_id);
    assert_eq!(authoritative.5, fixture.mapping_version);
    assert_eq!(authoritative.6, fixture.mapping_hash);
    assert_eq!(authoritative.7, fixture.validation_run_id);
    assert_eq!(authoritative.9, snapshot.snapshot_id);
    assert_eq!(authoritative.10, snapshot.snapshot_hash);
    assert_eq!(
        record.manifest.mapping_identity.as_deref(),
        Some(authoritative.4.as_str())
    );
    assert_eq!(
        record.manifest.mapping_version.as_deref(),
        Some(authoritative.5.as_str())
    );
    assert_eq!(
        record.manifest.validation_run_id.as_deref(),
        Some(authoritative.7.as_str())
    );
    assert_eq!(
        record.manifest.reconciliation_snapshot_id.as_deref(),
        Some(authoritative.9.as_str())
    );
    assert_eq!(
        record.manifest.approval_actor.as_deref(),
        Some("wp007-synthetic-approver")
    );
    assert_eq!(
        record.manifest.approval_reason.as_deref(),
        Some("Approve bounded synthetic candidate for WP-007 publication proof")
    );
    assert!(record.manifest.approval_occurred_at.is_some());

    let provenance = parse_json_member(&members, MEMBER_PROVENANCE);
    assert_eq!(provenance["contract_version"], PUBLICATION_CONTRACT_VERSION);
    assert_eq!(provenance["mapping_id"], authoritative.4);
    assert_eq!(provenance["mapping_version"], authoritative.5);
    assert_eq!(provenance["mapping_hash"], authoritative.6);
    assert_eq!(provenance["validation_run_id"], authoritative.7);
    assert_eq!(provenance["validation_result_hash"], authoritative.8);
    assert_eq!(
        provenance["reconciliation_policy_id"],
        "migration-stream/reconciliation-approval-policy/v1"
    );
    assert_non_placeholder_sha256(
        provenance["reconciliation_policy_hash"]
            .as_str()
            .expect("reconciliation policy hash"),
    );
    assert_non_placeholder_sha256(
        provenance["policy_activation_hash"]
            .as_str()
            .expect("policy activation hash"),
    );

    let reconciliation = parse_json_member(&members, MEMBER_RECONCILIATION_EVIDENCE);
    assert_eq!(
        reconciliation["reconciliation_snapshot_id"],
        authoritative.9
    );
    assert_eq!(
        reconciliation["reconciliation_snapshot_hash"],
        authoritative.10
    );
    assert_eq!(reconciliation["total_accepted_rows"], 4);
    assert_eq!(reconciliation["included_rows"], 3);
    assert_eq!(reconciliation["excluded_rows"], 1);
    assert_eq!(reconciliation["blocked_rows"], 0);
    assert_eq!(reconciliation["readiness_closure"], "ZERO_BLOCKERS");
    assert_eq!(
        reconciliation["category_counts"],
        serde_json::json!({
            "INCLUDED_VALID": 2,
            "INCLUDED_CORRECTED": 0,
            "INCLUDED_WAIVED": 0,
            "INCLUDED_ACCEPTED_AS_IS": 1,
            "EXCLUDED": 1,
            "BLOCKED_UNRESOLVED": 0,
            "BLOCKED_INCOMPLETE": 0,
            "BLOCKED_CONFLICT": 0,
        })
    );
    assert_eq!(
        reconciliation["readiness_summary"]["statement"],
        "All 4 accepted source rows are accounted for: 3 included in the local readiness extract, 1 excluded under governed decision evidence, and 0 blocked."
    );

    let approval = parse_json_member(&members, MEMBER_APPROVAL_EVIDENCE);
    assert_eq!(approval["candidate_id"], record.candidate_id);
    assert_eq!(approval["candidate_hash"], record.candidate_hash);
    assert_eq!(approval["approval_id"], record.approval_id);
    assert_eq!(approval["approval_hash"], record.approval_hash);
    assert_eq!(approval["action_kind"], "APPROVED");
    assert_eq!(approval["actor"], "wp007-synthetic-approver");
    assert_eq!(
        approval["approval_reason"],
        "Approve bounded synthetic candidate for WP-007 publication proof"
    );
    assert_eq!(
        approval["warning_acknowledgments"],
        serde_json::json!(["NON_BLOCKING_VALIDATION_WARNING"])
    );
    assert_eq!(approval["authority_boundary_acknowledged"], true);
    assert_eq!(
        approval["package_generation"]["practitioner"],
        "wp007-synthetic-publisher"
    );
    assert_eq!(
        approval["package_generation"]["reason"],
        "WP-007 bounded synthetic publication proof"
    );

    let decisions = parse_json_member(&members, MEMBER_ISSUE_DECISION_EVIDENCE);
    assert_eq!(decisions["decision_count"], 2);
    let decision_rows = decisions["decisions"]
        .as_array()
        .expect("exact decision evidence array");
    assert_eq!(
        decision_rows
            .iter()
            .map(|decision| decision["source_ordinal"]
                .as_u64()
                .expect("decision ordinal"))
            .collect::<Vec<_>>(),
        vec![3, 4]
    );
    assert_eq!(decision_rows[0]["disposition"], "ACCEPTED_AS_IS");
    assert_eq!(decision_rows[1]["disposition"], "EXCLUDED");
    for decision in decision_rows {
        assert_eq!(decision["issue_code"], "UNIQUE_DUPLICATE");
        assert_eq!(decision["practitioner"], "wp007-synthetic-decider");
        assert_eq!(
            decision["reason"],
            "Close blocking issue for WP-007 synthetic publication proof"
        );
        assert_non_placeholder_sha256(
            decision["issue_hash"]
                .as_str()
                .expect("decision issue hash"),
        );
        assert_non_placeholder_sha256(
            decision["decision_hash"]
                .as_str()
                .expect("decision evidence hash"),
        );
        assert!(
            decision["issue_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            decision["decision_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
    }

    let exclusions = parse_json_member(&members, MEMBER_EXCLUSION_EVIDENCE);
    assert_eq!(exclusions["excluded_count"], 1);
    assert_eq!(exclusions["excluded_rows"][0]["source_ordinal"], 4);
    assert_eq!(
        exclusions["excluded_rows"][0]["exclusion_decisions"][0]["disposition"],
        "EXCLUDED"
    );
    assert_eq!(
        exclusions["excluded_rows"][0]["exclusion_decisions"][0]["decision_id"],
        decision_rows[1]["decision_id"]
    );

    for identifier in [
        provenance["mapping_id"].as_str(),
        provenance["mapping_version"].as_str(),
        provenance["validation_run_id"].as_str(),
        reconciliation["reconciliation_snapshot_id"].as_str(),
    ] {
        assert_safe_governing_identifier(identifier.expect("governing identifier"));
    }
    assert!(
        [
            &record.manifest.source_package_digest,
            &record.manifest.profile_digest,
            &record.manifest.mapping_digest,
            &record.manifest.validation_digest,
            &record.manifest.reconciliation_snapshot_hash,
        ]
        .iter()
        .all(|digest| digest.as_str() != ZERO_SHA256),
        "governing manifest evidence must not use zero placeholders"
    );
    for digest in [
        provenance["mapping_hash"].as_str(),
        provenance["validation_result_hash"].as_str(),
        reconciliation["reconciliation_snapshot_hash"].as_str(),
    ] {
        assert_non_placeholder_sha256(digest.expect("governing digest"));
    }

    // --- package bounds ---
    assert!(
        record.package_size_bytes > 0,
        "generated package must have non-zero size"
    );
    assert!(
        record.package_size_bytes < migration_stream::publication::MAX_PACKAGE_SIZE_BYTES,
        "package must be within maximum size bound"
    );

    // Write proof evidence for the smoke script when WP007_PROOF_OUTPUT_DIR is set.
    if let Ok(output_dir) = std::env::var("WP007_PROOF_OUTPUT_DIR") {
        let output_path = std::path::Path::new(&output_dir);
        fs::create_dir_all(output_path).expect("create WP007_PROOF_OUTPUT_DIR for smoke evidence");
        let saved_zip_path = output_path.join("wp007-readiness.zip");
        fs::copy(&zip_path, &saved_zip_path).expect("copy completed ZIP to proof directory");
        let proof = serde_json::json!({
            "semantic_publication_hash": semantic,
            "physical_archive_digest": physical,
            "contract_version": record.contract_version,
            "package_size_bytes": record.package_size_bytes,
            "member_count": record.member_count,
            "archive_path": saved_zip_path.to_string_lossy(),
        });
        let proof_path = output_path.join("wp007-proof.json");
        fs::write(
            &proof_path,
            serde_json::to_string_pretty(&proof).expect("serialize proof JSON"),
        )
        .expect("write wp007-proof.json smoke evidence");
    }
}

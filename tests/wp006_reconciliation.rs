#![allow(clippy::default_trait_access, clippy::too_many_lines)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Barrier},
    thread,
};

use migration_stream::{
    KernelError,
    decision_read_model::{DecisionAwareIssueDetail, DecisionReadService},
    decision_service::DecisionService,
    decisions::{
        DECISION_SCHEMA_VERSION, DecisionConcurrency, DecisionScope, IssueDecisionRequest,
        IssueDisposition,
    },
    intake::ingest_package,
    mapping::validate_mapping,
    policy::IntakePolicy,
    profile_registry::{RegistryLimits, approve_profile, register_profile},
    read_model::PageRequest,
    reconciliation_approval::{
        RECONCILIATION_SCHEMA_VERSION, ResultActionKind, ResultActionPayload,
    },
    reconciliation_read_model::ReconciliationReadService,
    reconciliation_service::{
        CalculateReconciliationRequest, CreateCandidateRequest, ReconciliationApprovalService,
    },
    state::{integrity_check, recover_workspace},
    validation::{ValidationPolicy, run_validation},
};
use rusqlite::{Connection, OptionalExtension, params};
use tempfile::TempDir;

#[path = "support/object_workspace.rs"]
mod object_workspace_support;

const PROFILE_ID: &str = "synthetic.customer.readiness";
const PROFILE_VERSION: &str = "1.0.0";
const MAPPING_ID: &str = "synthetic.customer.valid";
const MAPPING_VERSION: &str = "1.0.0";
const CALCULATE_SCHEMA: &str = "migration-stream/write/calculate-reconciliation/v1";
const CANDIDATE_SCHEMA: &str = "migration-stream/write/create-result-candidate/v1";
const BOUNDARY_VERSION: &str = "migration-stream-result-boundary/v1";
const WARNING_CODE: &str = "NON_BLOCKING_VALIDATION_WARNING";

struct Fixture {
    temporary: TempDir,
    workspace: PathBuf,
    job_id: String,
    validation_run_id: String,
}

fn project_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn prepare_fixture() -> Fixture {
    let temporary = tempfile::tempdir_in(project_path("target"))
        .expect("link-safe temporary directory beneath target");
    let workspace = temporary.path().join("private-wp006-workspace");
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
        "WP-006 synthetic reconciliation integration",
        "Authorize only this synthetic profile for local WP-006 proof",
        true,
        &RegistryLimits::default(),
    )
    .expect("approve exact synthetic profile");
    let mapping_path = temporary.path().join("mapping.json");
    let mapping = fs::read_to_string(project_path("fixtures/wp003/valid-mapping.template.json"))
        .expect("read synthetic mapping template")
        .replace("__PROFILE_SHA256__", &approved.profile_hash);
    fs::write(&mapping_path, mapping).expect("write finalized synthetic mapping");
    validate_mapping(
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
        MAPPING_ID,
        MAPPING_VERSION,
        &project_path("schemas/migration-object-profile.schema.json"),
        &ValidationPolicy::from_path(&project_path("fixtures/wp003/validation-policy.json"))
            .expect("load bounded validation policy"),
    )
    .expect("create immutable source validation evidence");
    assert_eq!(validation.result.issue_count, 5);
    Fixture {
        temporary,
        workspace,
        job_id: intake.job_id,
        validation_run_id: validation.run_id,
    }
}

fn open_database(fixture: &Fixture) -> Connection {
    Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open authoritative synthetic state")
}

fn assert_wp006_integrity_failure(workspace: &Path, label: &str) {
    let report = integrity_check(workspace).expect("run tampered workspace integrity replay");
    assert!(!report.ok, "{label} unexpectedly passed integrity replay");
    assert!(
        !report.checked_reconciliation_approval,
        "{label} was not attributed to WP-006 integrity replay: {:?}",
        report.issues
    );
}

fn issue_scope(fixture: &Fixture, rule_id: &str, ordinal: u64) -> DecisionScope {
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
                i64::try_from(ordinal).expect("ordinal")
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
        .expect("select exact synthetic issue");
    assert_eq!(row.13, fixture.job_id);
    DecisionScope {
        job_id: fixture.job_id.clone(),
        validation_run_id: fixture.validation_run_id.clone(),
        issue_id: row.0,
        issue_hash: row.1,
        accepted_source_file: row.2,
        source_ordinal: ordinal,
        line_start: u64::try_from(row.3).expect("line start"),
        line_end: u64::try_from(row.4).expect("line end"),
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

fn issue_detail(fixture: &Fixture, scope: &DecisionScope) -> DecisionAwareIssueDetail {
    DecisionReadService::new(&fixture.workspace)
        .issue_detail(&fixture.job_id, &fixture.validation_run_id, &scope.issue_id)
        .expect("read exact issue authority")
}

fn decision_request(
    fixture: &Fixture,
    scope: DecisionScope,
    idempotency_key: &str,
    disposition: IssueDisposition,
) -> IssueDecisionRequest {
    let detail = issue_detail(fixture, &scope);
    IssueDecisionRequest {
        schema_version: DECISION_SCHEMA_VERSION,
        idempotency_key: idempotency_key.to_owned(),
        disposition,
        scope,
        concurrency: DecisionConcurrency {
            expected_active_decision_id: detail.data.concurrency.expected_active_decision_id,
            expected_active_decision_generation: detail.data.concurrency.expected_active_generation,
            expected_effective_value_hash: (disposition == IssueDisposition::AcceptedAsIs)
                .then_some(detail.data.concurrency.expected_effective_value_hash),
        },
        practitioner_id: "synthetic-practitioner-006".to_owned(),
        reason: format!("Bounded WP-006 synthetic {} evidence", disposition.as_str()),
        evidence_reference: Some("WP006-SYNTHETIC-EVIDENCE-ONLY".to_owned()),
        authority_separation_acknowledged: true,
        row_scope_exclusion_acknowledged: disposition == IssueDisposition::Excluded,
        correction_overlay: None,
    }
}

fn close_fixture_blockers(fixture: &Fixture) {
    let decisions = DecisionService::new(
        &fixture.workspace,
        project_path("schemas/migration-object-profile.schema.json"),
    );
    decisions
        .apply(&decision_request(
            fixture,
            issue_scope(fixture, "SYN-UNIQUE-001", 3),
            "wp006-close-row-three-0001",
            IssueDisposition::AcceptedAsIs,
        ))
        .expect("close row-three blocking issue");
    decisions
        .apply(&decision_request(
            fixture,
            issue_scope(fixture, "SYN-UNIQUE-001", 4),
            "wp006-exclude-row-four-0001",
            IssueDisposition::Excluded,
        ))
        .expect("exclude exact row-four scope");
}

fn calculate_request(fixture: &Fixture, key: &str) -> CalculateReconciliationRequest {
    CalculateReconciliationRequest {
        schema_version: CALCULATE_SCHEMA.to_owned(),
        job_id: fixture.job_id.clone(),
        validation_run_id: fixture.validation_run_id.clone(),
        practitioner_id: "synthetic-practitioner-006".to_owned(),
        reason: "Calculate bounded local readiness without publication authority".to_owned(),
        idempotency_key: key.to_owned(),
        local_readiness_only_acknowledged: true,
    }
}

fn candidate_request(
    fixture: &Fixture,
    snapshot_id: &str,
    snapshot_hash: &str,
    key: &str,
) -> CreateCandidateRequest {
    CreateCandidateRequest {
        schema_version: CANDIDATE_SCHEMA.to_owned(),
        job_id: fixture.job_id.clone(),
        snapshot_id: snapshot_id.to_owned(),
        snapshot_hash: snapshot_hash.to_owned(),
        practitioner_id: "synthetic-practitioner-006".to_owned(),
        reason: "Prepare exact local migration-result candidate only".to_owned(),
        idempotency_key: key.to_owned(),
        local_readiness_only_acknowledged: true,
    }
}

#[allow(clippy::too_many_arguments)]
fn action_payload(
    kind: ResultActionKind,
    candidate_id: &str,
    candidate_hash: &str,
    snapshot_id: &str,
    snapshot_hash: &str,
    predecessor: Option<(&str, &str)>,
    observed_generation: u64,
    key: &str,
    warnings: Vec<String>,
) -> ResultActionPayload {
    ResultActionPayload {
        schema_version: RECONCILIATION_SCHEMA_VERSION,
        kind,
        candidate_id: candidate_id.to_owned(),
        candidate_hash: candidate_hash.to_owned(),
        snapshot_id: snapshot_id.to_owned(),
        snapshot_hash: snapshot_hash.to_owned(),
        actor: "synthetic-practitioner-006".to_owned(),
        reason: format!("Bounded local {} action only", kind.as_str()),
        warning_acknowledgements: warnings,
        authority_boundary_acknowledged: true,
        authority_boundary_acknowledgement_version: BOUNDARY_VERSION.to_owned(),
        predecessor_action_id: predecessor.map(|value| value.0.to_owned()),
        predecessor_action_hash: predecessor.map(|value| value.1.to_owned()),
        observed_generation,
        idempotency_key: key.to_owned(),
    }
}

fn expect_code<T>(result: migration_stream::Result<T>, code: &str) -> KernelError {
    match result {
        Ok(_) => panic!("expected {code}"),
        Err(error) => {
            assert_eq!(error.code(), code, "unexpected error: {error}");
            error
        }
    }
}

#[test]
fn reconciliation_candidate_and_action_history_is_closed_current_and_append_only() {
    let fixture = prepare_fixture();
    let service = ReconciliationApprovalService::new(&fixture.workspace);
    let initial_request = calculate_request(&fixture, "wp006-calculate-initial-0001");
    let initial = service
        .calculate(&initial_request)
        .expect("initial reconciliation");
    assert_eq!(initial.accepted_row_count, 4);
    assert_eq!(initial.included_count, 2);
    assert_eq!(initial.excluded_count, 0);
    assert_eq!(initial.blocked_count, 2);
    assert!(!initial.publication_authority);
    let replay = service
        .calculate(&initial_request)
        .expect("exact calculation retry");
    assert!(replay.idempotent_replay);
    assert_eq!(replay.snapshot_hash, initial.snapshot_hash);
    let mut conflict_request = initial_request.clone();
    conflict_request.reason.push_str(" changed");
    expect_code(
        service.calculate(&conflict_request),
        "RESULT_APPROVAL_CONFLICT",
    );

    let blocked_candidate = service
        .create_candidate(&candidate_request(
            &fixture,
            &initial.snapshot_id,
            &initial.snapshot_hash,
            "wp006-candidate-blocked-0001",
        ))
        .expect("persist blocked candidate for transparent review");
    assert!(!blocked_candidate.eligible);
    assert_eq!(blocked_candidate.generation, 1);

    close_fixture_blockers(&fixture);

    let current = service
        .calculate(&calculate_request(&fixture, "wp006-calculate-current-0001"))
        .expect("calculate decision-aware current snapshot");
    assert_eq!(current.accepted_row_count, 4);
    assert_eq!(current.included_count, 3);
    assert_eq!(current.excluded_count, 1);
    assert_eq!(current.blocked_count, 0);
    let candidate = service
        .create_candidate(&candidate_request(
            &fixture,
            &current.snapshot_id,
            &current.snapshot_hash,
            "wp006-candidate-current-0001",
        ))
        .expect("create current eligible candidate");
    assert!(candidate.current);
    assert!(candidate.eligible);
    assert_eq!(candidate.generation, 2);

    let stale = ReconciliationReadService::new(&fixture.workspace)
        .approval_eligibility(&fixture.job_id, &blocked_candidate.candidate_id)
        .expect("derive stale predecessor candidate");
    assert!(!stale.data.current);
    assert!(!stale.data.eligible);
    assert!(
        stale
            .data
            .reason_codes
            .contains(&"STALE_GOVERNING_INPUTS".to_owned())
    );

    let approve_payload = action_payload(
        ResultActionKind::Approved,
        &candidate.candidate_id,
        &candidate.candidate_hash,
        &candidate.snapshot_id,
        &candidate.snapshot_hash,
        None,
        0,
        "wp006-action-approve-0001",
        vec![WARNING_CODE.to_owned()],
    );
    let approved = service
        .apply_action(&fixture.job_id, &approve_payload)
        .expect("approve exact current candidate");
    assert!(approved.active);
    assert_eq!(approved.generation, 1);
    assert!(!approved.publication_authority);
    assert!(!approved.authenticated_signature);
    let approval_replay = service
        .apply_action(&fixture.job_id, &approve_payload)
        .expect("exact approval retry");
    assert!(approval_replay.idempotent_replay);
    assert_eq!(approval_replay.action_hash, approved.action_hash);

    let competing_approval = action_payload(
        ResultActionKind::Approved,
        &candidate.candidate_id,
        &candidate.candidate_hash,
        &candidate.snapshot_id,
        &candidate.snapshot_hash,
        None,
        0,
        "wp006-action-competing-approve-0001",
        vec![WARNING_CODE.to_owned()],
    );
    expect_code(
        service.apply_action(&fixture.job_id, &competing_approval),
        "RESULT_APPROVAL_CONFLICT",
    );
    let persisted_action_count: i64 = open_database(&fixture)
        .query_row("SELECT COUNT(*) FROM migration_result_actions", [], |row| {
            row.get(0)
        })
        .expect("count the single authority winner");
    assert_eq!(persisted_action_count, 1);

    let mut conflicting_retry = approve_payload.clone();
    conflicting_retry.reason.push_str(" changed");
    expect_code(
        service.apply_action(&fixture.job_id, &conflicting_retry),
        "RESULT_APPROVAL_CONFLICT",
    );

    let reject_while_active = action_payload(
        ResultActionKind::Rejected,
        &candidate.candidate_id,
        &candidate.candidate_hash,
        &candidate.snapshot_id,
        &candidate.snapshot_hash,
        Some((&approved.action_id, &approved.action_hash)),
        approved.generation,
        "wp006-action-reject-active-0001",
        Vec::new(),
    );
    expect_code(
        service.apply_action(&fixture.job_id, &reject_while_active),
        "RESULT_APPROVAL_CONFLICT",
    );

    let revoked = service
        .apply_action(
            &fixture.job_id,
            &action_payload(
                ResultActionKind::ApprovalRevoked,
                &candidate.candidate_id,
                &candidate.candidate_hash,
                &candidate.snapshot_id,
                &candidate.snapshot_hash,
                Some((&approved.action_id, &approved.action_hash)),
                approved.generation,
                "wp006-action-revoke-0001",
                Vec::new(),
            ),
        )
        .expect("revoke exact active approval");
    assert!(!revoked.active);
    assert_eq!(revoked.generation, 2);

    let replacement = service
        .create_candidate(&candidate_request(
            &fixture,
            &current.snapshot_id,
            &current.snapshot_hash,
            "wp006-candidate-replacement-0001",
        ))
        .expect("create immutable replacement candidate");
    let reapproved = service
        .apply_action(
            &fixture.job_id,
            &action_payload(
                ResultActionKind::Approved,
                &candidate.candidate_id,
                &candidate.candidate_hash,
                &candidate.snapshot_id,
                &candidate.snapshot_hash,
                Some((&revoked.action_id, &revoked.action_hash)),
                revoked.generation,
                "wp006-action-reapprove-0001",
                vec![WARNING_CODE.to_owned()],
            ),
        )
        .expect("approve after explicit revocation head");
    let superseded = service
        .apply_action(
            &fixture.job_id,
            &action_payload(
                ResultActionKind::ApprovalSuperseded,
                &replacement.candidate_id,
                &replacement.candidate_hash,
                &replacement.snapshot_id,
                &replacement.snapshot_hash,
                Some((&reapproved.action_id, &reapproved.action_hash)),
                reapproved.generation,
                "wp006-action-supersede-0001",
                vec![WARNING_CODE.to_owned()],
            ),
        )
        .expect("supersede with exact eligible replacement");
    assert!(superseded.active);
    assert_eq!(superseded.candidate_id, replacement.candidate_id);
    assert_eq!(superseded.generation, 4);

    let action_history = ReconciliationReadService::new(&fixture.workspace)
        .action_history(&fixture.job_id, None, &PageRequest::default())
        .expect("read immutable action chain");
    assert_eq!(action_history.items.len(), 4);
    let integrity = integrity_check(&fixture.workspace).expect("replay all WP-006 evidence");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
    assert!(integrity.checked_reconciliation_approval);

    let connection = open_database(&fixture);
    let update_error = connection
        .execute(
            "UPDATE migration_result_actions SET reason = 'changed' WHERE action_id = ?1",
            [&superseded.action_id],
        )
        .expect_err("append-only action must reject update");
    assert!(update_error.to_string().contains("immutable"));
}

#[test]
fn competing_current_approvals_have_exactly_one_durable_authority_winner() {
    let fixture = prepare_fixture();
    close_fixture_blockers(&fixture);
    let service = ReconciliationApprovalService::new(&fixture.workspace);
    let snapshot = service
        .calculate(&calculate_request(
            &fixture,
            "wp006-concurrent-approval-snapshot-0001",
        ))
        .expect("calculate current concurrent-approval snapshot");
    let candidate = service
        .create_candidate(&candidate_request(
            &fixture,
            &snapshot.snapshot_id,
            &snapshot.snapshot_hash,
            "wp006-concurrent-approval-candidate-0001",
        ))
        .expect("create eligible concurrent-approval candidate");
    assert!(candidate.eligible);

    let payloads = [
        action_payload(
            ResultActionKind::Approved,
            &candidate.candidate_id,
            &candidate.candidate_hash,
            &candidate.snapshot_id,
            &candidate.snapshot_hash,
            None,
            0,
            "wp006-concurrent-approval-action-0001",
            vec![WARNING_CODE.to_owned()],
        ),
        action_payload(
            ResultActionKind::Approved,
            &candidate.candidate_id,
            &candidate.candidate_hash,
            &candidate.snapshot_id,
            &candidate.snapshot_hash,
            None,
            0,
            "wp006-concurrent-approval-action-0002",
            vec![WARNING_CODE.to_owned()],
        ),
    ];
    let barrier = Arc::new(Barrier::new(3));
    let results = thread::scope(|scope| {
        let handles = payloads
            .into_iter()
            .map(|payload| {
                let service = service.clone();
                let job_id = fixture.job_id.clone();
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    service.apply_action(&job_id, &payload)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("competing approval thread"))
            .collect::<Vec<_>>()
    });
    let winners = results.iter().filter(|result| result.is_ok()).count();
    assert_eq!(winners, 1);
    for error in results.iter().filter_map(|result| result.as_ref().err()) {
        assert!(
            matches!(
                error.code(),
                "RESULT_APPROVAL_CONFLICT" | "WORKSPACE_LOCKED"
            ),
            "unexpected competing-action error: {error}"
        );
    }
    let connection = open_database(&fixture);
    let action_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM migration_result_actions", [], |row| {
            row.get(0)
        })
        .expect("count durable authority winner");
    assert_eq!(action_count, 1);
    drop(connection);
    let report = integrity_check(&fixture.workspace).expect("replay concurrency winner");
    assert!(
        report.ok,
        "concurrency winner did not replay: {:?}",
        report.issues
    );
}

#[test]
fn failed_action_commit_grants_no_authority_and_exact_retry_commits_once() {
    let fixture = prepare_fixture();
    let service = ReconciliationApprovalService::new(&fixture.workspace);
    let snapshot = service
        .calculate(&calculate_request(
            &fixture,
            "wp006-action-atomic-snapshot-0001",
        ))
        .expect("calculate action-atomicity snapshot");
    let candidate = service
        .create_candidate(&candidate_request(
            &fixture,
            &snapshot.snapshot_id,
            &snapshot.snapshot_hash,
            "wp006-action-atomic-candidate-0001",
        ))
        .expect("create action-atomicity candidate");
    let payload = action_payload(
        ResultActionKind::Rejected,
        &candidate.candidate_id,
        &candidate.candidate_hash,
        &candidate.snapshot_id,
        &candidate.snapshot_hash,
        None,
        0,
        "wp006-action-atomic-rejection-0001",
        Vec::new(),
    );
    let connection = open_database(&fixture);
    connection
        .execute_batch(
            "CREATE TRIGGER wp006_test_abort_action
             BEFORE INSERT ON migration_result_actions
             BEGIN
                 SELECT RAISE(ABORT, 'synthetic action persistence failure');
             END;",
        )
        .expect("install bounded action-failure trigger");
    drop(connection);

    expect_code(service.apply_action(&fixture.job_id, &payload), "SQLITE");
    let connection = open_database(&fixture);
    let action_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM migration_result_actions", [], |row| {
            row.get(0)
        })
        .expect("count absent partial actions");
    assert_eq!(action_count, 0);
    connection
        .execute_batch("DROP TRIGGER wp006_test_abort_action;")
        .expect("remove bounded action-failure trigger");
    drop(connection);

    let retry = service
        .apply_action(&fixture.job_id, &payload)
        .expect("retry exact action after rolled-back insert");
    assert_eq!(retry.generation, 1);
    assert!(!retry.active);
    let replay = service
        .apply_action(&fixture.job_id, &payload)
        .expect("replay exact committed action");
    assert!(replay.idempotent_replay);
    let report = integrity_check(&fixture.workspace).expect("replay action atomicity proof");
    assert!(
        report.ok,
        "action atomicity did not replay: {:?}",
        report.issues
    );
}

#[test]
fn failed_snapshot_commit_records_one_terminal_nonqualifying_attempt() {
    let fixture = prepare_fixture();
    open_database(&fixture)
        .execute_batch(
            "CREATE TRIGGER wp006_test_abort_snapshot
             BEFORE INSERT ON reconciliation_snapshots
             BEGIN
                 SELECT RAISE(ABORT, 'synthetic snapshot persistence failure');
             END;",
        )
        .expect("install bounded snapshot-failure trigger");

    expect_code(
        ReconciliationApprovalService::new(&fixture.workspace).calculate(&calculate_request(
            &fixture,
            "wp006-terminal-failure-calculate-0001",
        )),
        "SQLITE",
    );

    let connection = open_database(&fixture);
    let (state, error_code, snapshot_count): (String, String, i64) = connection
        .query_row(
            "SELECT e.state, e.error_code,
                    (SELECT COUNT(*) FROM reconciliation_snapshots)
             FROM reconciliation_attempt_events e
             WHERE e.attempt_id = (SELECT attempt_id FROM reconciliation_attempts)
             ORDER BY e.sequence DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read terminal failed reconciliation evidence");
    assert_eq!(state, "FAILED");
    assert_eq!(error_code, "SQLITE");
    assert_eq!(snapshot_count, 0);
    drop(connection);

    let report = integrity_check(&fixture.workspace).expect("replay failed attempt evidence");
    assert!(
        report.ok,
        "failed attempt did not replay: {:?}",
        report.issues
    );
}

#[test]
fn candidate_insert_failure_is_atomic_and_retry_does_not_reuse_partial_history() {
    let fixture = prepare_fixture();
    let service = ReconciliationApprovalService::new(&fixture.workspace);
    let snapshot = service
        .calculate(&calculate_request(
            &fixture,
            "wp006-candidate-atomic-snapshot-0001",
        ))
        .expect("calculate candidate atomicity snapshot");
    let request = candidate_request(
        &fixture,
        &snapshot.snapshot_id,
        &snapshot.snapshot_hash,
        "wp006-candidate-atomic-insert-0001",
    );
    open_database(&fixture)
        .execute_batch(
            "CREATE TRIGGER wp006_test_abort_candidate
             BEFORE INSERT ON migration_result_candidates
             BEGIN
                 SELECT RAISE(ABORT, 'synthetic candidate persistence failure');
             END;",
        )
        .expect("install bounded candidate-failure trigger");

    expect_code(service.create_candidate(&request), "SQLITE");
    let connection = open_database(&fixture);
    let candidate_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM migration_result_candidates",
            [],
            |row| row.get(0),
        )
        .expect("count absent partial candidates");
    assert_eq!(candidate_count, 0);
    connection
        .execute_batch("DROP TRIGGER wp006_test_abort_candidate;")
        .expect("remove bounded candidate-failure trigger");
    drop(connection);

    let retry = service
        .create_candidate(&request)
        .expect("retry exact candidate after rolled-back insert");
    assert_eq!(retry.generation, 1);
    assert!(!retry.eligible);
    let report = integrity_check(&fixture.workspace).expect("replay atomic candidate retry");
    assert!(
        report.ok,
        "candidate retry did not replay: {:?}",
        report.issues
    );
}

#[test]
fn trigger_bypass_tampering_of_rows_counts_candidates_and_action_chain_fails_replay() {
    let fixture = prepare_fixture();
    let service = ReconciliationApprovalService::new(&fixture.workspace);
    let snapshot = service
        .calculate(&calculate_request(
            &fixture,
            "wp006-tamper-proof-snapshot-0001",
        ))
        .expect("calculate tamper-proof snapshot");
    let candidate = service
        .create_candidate(&candidate_request(
            &fixture,
            &snapshot.snapshot_id,
            &snapshot.snapshot_hash,
            "wp006-tamper-proof-candidate-0001",
        ))
        .expect("create tamper-proof candidate");
    service
        .apply_action(
            &fixture.job_id,
            &action_payload(
                ResultActionKind::Rejected,
                &candidate.candidate_id,
                &candidate.candidate_hash,
                &candidate.snapshot_id,
                &candidate.snapshot_hash,
                None,
                0,
                "wp006-tamper-proof-rejection-0001",
                Vec::new(),
            ),
        )
        .expect("persist exact rejection evidence");

    let database_path = fixture.workspace.join("state.sqlite3");
    let pristine_path = fixture.temporary.path().join("wp006-pristine.sqlite3");
    open_database(&fixture)
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("checkpoint bounded tamper fixture");
    fs::copy(&database_path, &pristine_path).expect("copy pristine bounded database");

    let tamper_cases = [
        (
            "row classification",
            "DROP TRIGGER reconciliation_row_classifications_no_update;
             UPDATE reconciliation_row_classifications
             SET category = CASE category
                 WHEN 'EXCLUDED' THEN 'INCLUDED_VALID' ELSE 'EXCLUDED' END
             WHERE rowid = (SELECT rowid FROM reconciliation_row_classifications LIMIT 1);
             CREATE TRIGGER reconciliation_row_classifications_no_update
             BEFORE UPDATE ON reconciliation_row_classifications
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation row-classification history is immutable');
             END;",
        ),
        (
            "category count",
            "DROP TRIGGER reconciliation_category_counts_no_update;
             UPDATE reconciliation_category_counts SET row_count = row_count + 1
             WHERE rowid = (SELECT rowid FROM reconciliation_category_counts LIMIT 1);
             CREATE TRIGGER reconciliation_category_counts_no_update
             BEFORE UPDATE ON reconciliation_category_counts
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation category-count history is immutable');
             END;",
        ),
        (
            "snapshot warning material",
            "DROP TRIGGER reconciliation_snapshots_no_update;
             UPDATE reconciliation_snapshots SET warning_codes_json = '[\"FORGED\"]';
             CREATE TRIGGER reconciliation_snapshots_no_update
             BEFORE UPDATE ON reconciliation_snapshots
             BEGIN
                 SELECT RAISE(ABORT, 'reconciliation snapshot history is immutable');
             END;",
        ),
        (
            "candidate",
            "DROP TRIGGER migration_result_candidates_no_update;
             UPDATE migration_result_candidates SET creator = creator || '-tampered';
             CREATE TRIGGER migration_result_candidates_no_update
             BEFORE UPDATE ON migration_result_candidates
             BEGIN
                 SELECT RAISE(ABORT, 'migration-result candidate history is immutable');
             END;",
        ),
        (
            "action actor",
            "DROP TRIGGER migration_result_actions_no_update;
             UPDATE migration_result_actions SET actor = actor || '-tampered';
             CREATE TRIGGER migration_result_actions_no_update
             BEFORE UPDATE ON migration_result_actions
             BEGIN
                 SELECT RAISE(ABORT, 'migration-result action history is immutable');
             END;",
        ),
        (
            "action reason",
            "DROP TRIGGER migration_result_actions_no_update;
             UPDATE migration_result_actions SET reason = reason || '-tampered';
             CREATE TRIGGER migration_result_actions_no_update
             BEFORE UPDATE ON migration_result_actions
             BEGIN
                 SELECT RAISE(ABORT, 'migration-result action history is immutable');
             END;",
        ),
        (
            "action warning acknowledgments",
            "DROP TRIGGER migration_result_actions_no_update;
             UPDATE migration_result_actions
             SET warning_acknowledgments_json = '[\"FORGED\"]';
             CREATE TRIGGER migration_result_actions_no_update
             BEFORE UPDATE ON migration_result_actions
             BEGIN
                 SELECT RAISE(ABORT, 'migration-result action history is immutable');
             END;",
        ),
        (
            "action predecessor chain",
            "PRAGMA foreign_keys = OFF;
             DROP TRIGGER migration_result_actions_no_update;
             UPDATE migration_result_actions
             SET predecessor_action_id = 'forged-predecessor',
                 predecessor_generation = 1, observed_generation = 1, generation = 2;
             CREATE TRIGGER migration_result_actions_no_update
             BEFORE UPDATE ON migration_result_actions
             BEGIN
                 SELECT RAISE(ABORT, 'migration-result action history is immutable');
             END;",
        ),
        (
            "action chain",
            "DROP TRIGGER migration_result_actions_no_update;
             UPDATE migration_result_actions
             SET action_hash = '0000000000000000000000000000000000000000000000000000000000000000';
             CREATE TRIGGER migration_result_actions_no_update
             BEFORE UPDATE ON migration_result_actions
             BEGIN
                 SELECT RAISE(ABORT, 'migration-result action history is immutable');
             END;",
        ),
    ];
    for (label, sql) in tamper_cases {
        fs::copy(&pristine_path, &database_path).expect("restore pristine bounded database");
        Connection::open(&database_path)
            .expect("open bounded tamper database")
            .execute_batch(sql)
            .expect("apply bounded trigger-bypass tamper");
        assert_wp006_integrity_failure(&fixture.workspace, label);
    }
}

#[test]
#[ignore = "subprocess crash driver; invoked only by the hard-abort parent regression"]
fn hard_abort_reconciliation_after_running_child_driver() {
    let workspace = PathBuf::from(
        std::env::var("MIGRATION_STREAM_TEST_RECONCILIATION_WORKSPACE").expect("child workspace"),
    );
    let request: CalculateReconciliationRequest = serde_json::from_str(
        &std::env::var("MIGRATION_STREAM_TEST_RECONCILIATION_REQUEST_JSON").expect("child request"),
    )
    .expect("parse child calculation request");
    let result = ReconciliationApprovalService::new(workspace).calculate(&request);
    panic!("crash injection returned instead of aborting: {result:?}");
}

#[test]
fn hard_abort_running_reconciliation_recovers_once_and_never_claims_completion() {
    let fixture = prepare_fixture();
    let request = calculate_request(&fixture, "wp006-hard-abort-calculate-0001");
    let executable = std::env::current_exe().expect("resolve integration-test executable");
    let child = Command::new(executable)
        .arg("--ignored")
        .arg("--exact")
        .arg("hard_abort_reconciliation_after_running_child_driver")
        .arg("--nocapture")
        .env(
            "MIGRATION_STREAM_TEST_CRASH_RECONCILIATION_AFTER_RUNNING",
            "1",
        )
        .env(
            "MIGRATION_STREAM_TEST_RECONCILIATION_WORKSPACE",
            &fixture.workspace,
        )
        .env(
            "MIGRATION_STREAM_TEST_RECONCILIATION_REQUEST_JSON",
            serde_json::to_string(&request).expect("serialize child request"),
        )
        .output()
        .expect("spawn crash-injected reconciliation child");
    assert!(!child.status.success());
    #[cfg(unix)]
    assert!(
        child.status.code().is_none(),
        "child did not abort: {:?}",
        child.status
    );

    let connection = open_database(&fixture);
    let (attempt_id, state): (String, String) = connection
        .query_row(
            "SELECT a.attempt_id,
                    (SELECT e.state FROM reconciliation_attempt_events e
                     WHERE e.attempt_id = a.attempt_id
                     ORDER BY e.sequence DESC LIMIT 1)
             FROM reconciliation_attempts a",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read crash attempt");
    assert_eq!(state, "RUNNING");
    let snapshot: Option<String> = connection
        .query_row(
            "SELECT snapshot_id FROM reconciliation_snapshots WHERE attempt_id = ?1",
            [&attempt_id],
            |row| row.get(0),
        )
        .optional()
        .expect("query absent snapshot");
    assert_eq!(snapshot, None);
    drop(connection);

    let first = recover_workspace(&fixture.workspace).expect("recover interrupted reconciliation");
    assert_eq!(first.recovered_reconciliation_attempts, 1);
    let repeated = recover_workspace(&fixture.workspace).expect("repeat idempotent recovery");
    assert_eq!(repeated.recovered_reconciliation_attempts, 0);
    let state: String = open_database(&fixture)
        .query_row(
            "SELECT state FROM reconciliation_attempt_events
             WHERE attempt_id = ?1 ORDER BY sequence DESC LIMIT 1",
            [&attempt_id],
            |row| row.get(0),
        )
        .expect("read recovered terminal state");
    assert_eq!(state, "INTERRUPTED");

    expect_code(
        ReconciliationApprovalService::new(&fixture.workspace).calculate(&request),
        "RESULT_APPROVAL_CONFLICT",
    );
    let rerun = ReconciliationApprovalService::new(&fixture.workspace)
        .calculate(&calculate_request(&fixture, "wp006-hard-abort-rerun-0002"))
        .expect("new-key rerun after interrupted history");
    assert_eq!(rerun.accepted_row_count, 4);
    let integrity =
        integrity_check(&fixture.workspace).expect("replay interrupted and complete attempts");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

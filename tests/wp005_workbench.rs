use std::{
    fs,
    path::{Path, PathBuf},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header::HOST},
};
use jsonschema::PatternOptions;
use migration_stream::{
    intake::ingest_package,
    mapping::validate_mapping,
    policy::IntakePolicy,
    profile_registry::{RegistryLimits, approve_profile, register_profile},
    profiling::{ProfilingPolicy, run_profile},
    read_model::SelectedGovernance,
    validation::{ValidationPolicy, run_validation},
    workbench::{WorkbenchConfig, build_application_router, build_router},
};
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt as _;

const PROFILE_ID: &str = "synthetic.customer.readiness";
const PROFILE_VERSION: &str = "1.0.0";
const MAPPING_ID: &str = "synthetic.customer.valid";
const MAPPING_VERSION: &str = "1.0.0";

struct WorkbenchFixture {
    temporary: TempDir,
    workspace: PathBuf,
    job_id: String,
    profiling_run_id: String,
}

struct JsonResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

fn project_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn prepare_fixture() -> WorkbenchFixture {
    let temporary = tempfile::tempdir_in(project_path("target"))
        .expect("link-safe temporary directory beneath target");
    let workspace = temporary.path().join("wp005-workbench");
    fs::create_dir(&workspace).expect("create selected object workspace");
    migration_stream::object_workspace::establish_for_local_automation(
        &workspace,
        "Synthetic WP-005 workspace",
        "Customer decisions",
        "Automated workbench proof",
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
    .expect("register synthetic profile");
    let approved = approve_profile(
        &workspace,
        PROFILE_ID,
        PROFILE_VERSION,
        &registered.profile_hash,
        "WP-005 workbench test",
        "Authorize this exact synthetic profile for local decision proof only",
        true,
        &RegistryLimits::default(),
    )
    .expect("approve exact synthetic profile");
    let mapping_path = temporary.path().join("mapping.json");
    let mapping_template =
        fs::read_to_string(project_path("fixtures/wp003/valid-mapping.template.json"))
            .expect("read mapping template");
    fs::write(
        &mapping_path,
        mapping_template.replace("__PROFILE_SHA256__", &approved.profile_hash),
    )
    .expect("write synthetic mapping");
    validate_mapping(
        &workspace,
        &intake.job_id,
        &project_path("schemas/source-mapping-version.schema.json"),
        &project_path("schemas/migration-object-profile.schema.json"),
        &mapping_path,
    )
    .expect("validate synthetic mapping");
    let profiling = run_profile(
        &workspace,
        &intake.job_id,
        &ProfilingPolicy {
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
        },
    )
    .expect("profile synthetic source");
    let validation_policy =
        ValidationPolicy::from_path(&project_path("fixtures/wp003/validation-policy.json"))
            .expect("load synthetic validation policy");
    run_validation(
        &workspace,
        &intake.job_id,
        MAPPING_ID,
        MAPPING_VERSION,
        &project_path("schemas/migration-object-profile.schema.json"),
        &validation_policy,
    )
    .expect("run bounded validation");
    WorkbenchFixture {
        temporary,
        workspace,
        job_id: intake.job_id,
        profiling_run_id: profiling.run_id,
    }
}

fn router(fixture: &WorkbenchFixture, governed: bool) -> Router {
    let mut config = WorkbenchConfig::new(
        &fixture.workspace,
        fixture.job_id.clone(),
        Some(fixture.profiling_run_id.clone()),
    );
    if governed {
        config = config.with_governed_decisions();
    }
    build_router(config).expect("preflight WP-005 workbench")
}

async fn request_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    governed_headers: Option<(&str, &str)>,
) -> JsonResponse {
    let bytes = body
        .map(|document| serde_json::to_vec(&document))
        .transpose()
        .expect("serialize bounded request")
        .unwrap_or_default();
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(HOST, "127.0.0.1:4173");
    if let Some((idempotency_key, origin)) = governed_headers {
        request = request
            .header("origin", origin)
            .header("content-type", "application/json")
            .header("idempotency-key", idempotency_key)
            .header("x-migration-stream-intent", "governed-decision-v1")
            .header("content-length", bytes.len());
    }
    json_response(
        app,
        request
            .body(Body::from(bytes))
            .expect("bounded test request"),
    )
    .await
}

async fn json_response(app: &Router, request: Request<Body>) -> JsonResponse {
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("infallible router response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 70 * 1024 * 1024)
        .await
        .expect("bounded response");
    let body = serde_json::from_slice(&bytes).expect("JSON response");
    JsonResponse {
        status,
        headers,
        body,
    }
}

fn request_with_host(method: Method, uri: &str, host: Option<&str>) -> Request<Body> {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(host) = host {
        request = request.header(HOST, host);
    }
    request.body(Body::empty()).expect("bounded test request")
}

fn error_code(response: &JsonResponse) -> &str {
    response.body["error"]["code"]
        .as_str()
        .expect("stable error code")
}

fn validate_transport_schema(document: &Value, relative_schema: &str) {
    let schema: Value = serde_json::from_slice(
        &fs::read(project_path(relative_schema)).expect("read transport schema"),
    )
    .expect("parse transport schema");
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .with_pattern_options(PatternOptions::fancy_regex())
        .build(&schema)
        .expect("compile transport schema");
    let errors = validator
        .iter_errors(document)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "{relative_schema} rejected response: {}",
        errors.join("\n")
    );
}

#[tokio::test]
async fn every_surface_rejects_missing_duplicate_invalid_or_non_loopback_host() {
    let fixture = prepare_fixture();
    let app = router(&fixture, false);

    let missing = json_response(
        &app,
        request_with_host(Method::GET, "/api/v1/context", None),
    )
    .await;
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&missing), "INVALID_WORKBENCH_HOST");

    let mut duplicate = request_with_host(Method::GET, "/api/v1/context", Some("127.0.0.1:4173"));
    duplicate
        .headers_mut()
        .append(HOST, HeaderValue::from_static("localhost:4173"));
    let duplicate = json_response(&app, duplicate).await;
    assert_eq!(duplicate.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&duplicate), "INVALID_WORKBENCH_HOST");

    for host in [
        "127.0.0.1:not-a-port",
        "127.0.0.1:",
        "127.0.0.1:65536",
        "attacker.example:4173",
    ] {
        let response = json_response(
            &app,
            request_with_host(Method::GET, "/api/v1/context", Some(host)),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "Host {host}");
        assert_eq!(error_code(&response), "INVALID_WORKBENCH_HOST");
    }

    let localhost = json_response(
        &app,
        request_with_host(Method::GET, "/api/v1/context", Some("localhost:4173")),
    )
    .await;
    assert_eq!(localhost.status, StatusCode::OK);

    let ipv6_loopback = json_response(
        &app,
        request_with_host(Method::GET, "/api/v1/context", Some("[::1]:4173")),
    )
    .await;
    assert_eq!(ipv6_loopback.status, StatusCode::OK);

    let assets = fixture.temporary.path().join("host-gate-assets");
    fs::create_dir(&assets).expect("create bounded static assets");
    fs::write(
        assets.join("index.html"),
        "<!doctype html><title>host gate</title>",
    )
    .expect("write bounded static index");
    let static_app = build_application_router(
        WorkbenchConfig::new(
            &fixture.workspace,
            fixture.job_id.clone(),
            Some(fixture.profiling_run_id.clone()),
        ),
        &assets,
    )
    .expect("preflight static workbench");
    let static_rebinding = json_response(
        &static_app,
        request_with_host(Method::GET, "/", Some("attacker.example:4173")),
    )
    .await;
    assert_eq!(static_rebinding.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&static_rebinding), "INVALID_WORKBENCH_HOST");
}

#[tokio::test]
async fn governed_posts_reject_the_forwarded_header_family_before_domain_work() {
    let fixture = prepare_fixture();
    let app = router(&fixture, true);
    let endpoint = "/api/v1/issues/synthetic-issue/decisions/waive";

    for (index, header) in [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-port",
        "x-forwarded-prefix",
        "x-forwarded-proto",
        "x-forwarded-server",
    ]
    .into_iter()
    .enumerate()
    {
        let body = b"{}";
        let request = Request::builder()
            .method(Method::POST)
            .uri(endpoint)
            .header(HOST, "127.0.0.1:4173")
            .header("origin", "http://127.0.0.1:4173")
            .header("content-type", "application/json")
            .header("idempotency-key", format!("forwarded-rejection-{index:04}"))
            .header("x-migration-stream-intent", "governed-decision-v1")
            .header("content-length", body.len())
            .header(header, "untrusted-proxy-evidence")
            .body(Body::from(body.as_slice()))
            .expect("bounded forwarded-header request");
        let response = json_response(&app, request).await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "header {header}");
        assert_eq!(error_code(&response), "INVALID_GOVERNED_TRANSPORT");
    }

    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("inspect rejected forwarded requests");
    let attempts: i64 = connection
        .query_row("SELECT COUNT(*) FROM decision_attempts", [], |row| {
            row.get(0)
        })
        .expect("rejected attempt count");
    assert_eq!(attempts, 0, "rejected proxy headers reached domain writes");
}

#[tokio::test]
async fn read_only_mode_exposes_no_governed_authority() {
    let fixture = prepare_fixture();
    let app = router(&fixture, false);
    let context = request_json(&app, Method::GET, "/api/v1/context", None, None).await;
    assert_eq!(context.status, StatusCode::OK);
    validate_transport_schema(&context.body, "schemas/http/workbench-context.schema.json");
    assert_eq!(
        context.body["schema_version"],
        "migration-stream/workbench/context/v1"
    );
    assert_eq!(context.body["read_only"], true);
    assert_eq!(
        context.body["supported_contracts"]
            .as_array()
            .expect("contracts")
            .len(),
        10
    );
    assert!(context.body.get("capabilities").is_none());
    let journey = request_json(&app, Method::GET, "/api/v1/journey", None, None).await;
    assert_eq!(journey.status, StatusCode::OK);
    assert_eq!(journey.body["schemaVersion"], "migration-stream/journey/v2");
    assert_eq!(journey.body["currentStageId"], "ISSUES");
    assert_eq!(journey.body["binding"]["jobId"], fixture.job_id);
    assert_eq!(journey.body["stages"][7]["status"], "LOCKED");
    assert_eq!(journey.body["stages"][7]["href"], Value::Null);
    assert_eq!(journey.body["stages"][8]["status"], "AVAILABLE");
    assert_eq!(journey.body["stages"][8]["href"], "/reconciliation");
    assert_eq!(journey.body["stages"][9]["status"], "LOCKED");

    let queue = request_json(
        &app,
        Method::GET,
        "/api/v1/decision-queue?page_size=200",
        None,
        None,
    )
    .await;
    assert_eq!(queue.status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&queue), "GOVERNED_DECISIONS_DISABLED");

    let mutation = request_json(
        &app,
        Method::POST,
        "/api/v1/issues/synthetic-issue/decisions/waive",
        Some(json!({})),
        None,
    )
    .await;
    assert_eq!(mutation.status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&mutation), "GOVERNED_DECISIONS_DISABLED");
}

#[test]
fn exact_engagement_governance_rejects_a_different_selected_run() {
    let fixture = prepare_fixture();
    let expected = SelectedGovernance {
        profile_id: PROFILE_ID.to_owned(),
        profile_version: PROFILE_VERSION.to_owned(),
        profile_hash: "a".repeat(64),
        mapping_id: MAPPING_ID.to_owned(),
        mapping_version: MAPPING_VERSION.to_owned(),
        mapping_hash: "b".repeat(64),
        validation_run_id: "stale-active-validation-run".to_owned(),
        validation_state: "COMPLETE".to_owned(),
    };
    let result = build_router(
        WorkbenchConfig::new(
            &fixture.workspace,
            fixture.job_id,
            Some(fixture.profiling_run_id),
        )
        .with_expected_governance(Some(expected))
        .with_governed_decisions(),
    );
    assert!(matches!(
        result,
        Err(migration_stream::KernelError::IntegrityFailed(_))
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn governed_mode_enforces_transport_scope_and_returns_durable_results() {
    let fixture = prepare_fixture();
    let app = router(&fixture, true);
    let context = request_json(&app, Method::GET, "/api/v1/context", None, None).await;
    assert_eq!(context.status, StatusCode::OK);
    validate_transport_schema(
        &context.body,
        "schemas/http/workbench-context-v2.schema.json",
    );
    assert_eq!(
        context.body["schema_version"],
        "migration-stream/workbench/context/v2"
    );
    assert_eq!(context.body["read_only"], false);
    assert_eq!(context.body["capabilities"]["source_mutation"], false);
    assert_eq!(context.body["capabilities"]["publication"], false);
    assert_eq!(
        context.body["supported_contracts"]
            .as_array()
            .expect("contracts")
            .len(),
        20
    );
    let journey = request_json(&app, Method::GET, "/api/v1/journey", None, None).await;
    assert_eq!(journey.status, StatusCode::OK);
    assert_eq!(journey.body["currentStageId"], "DECISIONS");
    assert_eq!(journey.body["stages"][7]["href"], "/decisions");
    assert_eq!(journey.body["stages"][8]["href"], "/reconciliation");
    assert_eq!(journey.body["stages"][9]["status"], "LOCKED");

    let queue = request_json(
        &app,
        Method::GET,
        "/api/v1/decision-queue?page_size=1",
        None,
        None,
    )
    .await;
    assert_eq!(queue.status, StatusCode::OK);
    validate_transport_schema(&queue.body, "schemas/read/decision-queue-page.schema.json");
    assert_eq!(
        queue.body["schema_version"],
        "migration-stream/read/decision-queue/v1"
    );
    let unsupported_filter = request_json(
        &app,
        Method::GET,
        "/api/v1/decision-queue?disposition=DELETE_SOURCE",
        None,
        None,
    )
    .await;
    assert_eq!(unsupported_filter.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&unsupported_filter), "INVALID_REQUEST");
    let broad_queue = queue.body.to_string();
    assert!(!broad_queue.contains("000001"));
    assert!(!broad_queue.contains("Acme, S.A."));
    let mut selected = None;
    for item in queue.body["items"].as_array().expect("synthetic issues") {
        let candidate = item["issue_id"].as_str().expect("issue ID").to_owned();
        let detail = request_json(
            &app,
            Method::GET,
            &format!("/api/v1/issues/{candidate}/decision-detail"),
            None,
            None,
        )
        .await;
        assert_eq!(detail.status, StatusCode::OK);
        if detail.body["data"]["field_context"]["correction_eligible"] == true {
            selected = Some((candidate, detail));
            break;
        }
    }
    let (issue_id, detail) = selected.expect("one correction-eligible synthetic issue");
    validate_transport_schema(
        &detail.body,
        "schemas/read/decision-aware-issue-detail.schema.json",
    );
    let data = &detail.body["data"];
    let waiver = json!({
        "schema_version": "migration-stream/write/waive-issue/v1",
        "validation_run_id": data["validation_run_id"],
        "issue_hash": data["issue_hash"],
        "practitioner_identifier": "synthetic-practitioner",
        "reason": "Bounded synthetic waiver evidence",
        "evidence_reference": null,
        "expected_active_decision_id": null,
        "expected_active_generation": 0,
        "acknowledge_not_migration_result_approval": true,
        "acknowledge_waiver_limitations": true
    });
    let endpoint = format!("/api/v1/issues/{issue_id}/decisions/waive");

    let missing_origin = request_json(
        &app,
        Method::POST,
        &endpoint,
        Some(waiver.clone()),
        Some(("decision-transport-0001", "http://wrong.example")),
    )
    .await;
    assert_eq!(missing_origin.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&missing_origin), "INVALID_GOVERNED_TRANSPORT");

    let stale_scope = request_json(
        &app,
        Method::POST,
        &endpoint,
        Some({
            let mut stale = waiver.clone();
            stale["issue_hash"] = Value::String("0".repeat(64));
            stale
        }),
        Some(("decision-transport-0002", "http://127.0.0.1:4173")),
    )
    .await;
    assert_eq!(stale_scope.status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&stale_scope), "INVALID_DECISION_COMMAND");

    let committed = request_json(
        &app,
        Method::POST,
        &endpoint,
        Some(waiver.clone()),
        Some(("decision-transport-0003", "http://127.0.0.1:4173")),
    )
    .await;
    assert_eq!(committed.status, StatusCode::OK, "{}", committed.body);
    validate_transport_schema(
        &committed.body,
        "schemas/write/decision-command-result.schema.json",
    );
    assert_eq!(
        committed.body["schema_version"],
        "migration-stream/write/decision-command-result/v1"
    );
    assert_eq!(committed.body["data"]["disposition"], "WAIVED");
    assert_eq!(committed.body["data"]["active"], true);
    assert_eq!(committed.body["data"]["idempotent_replay"], false);
    let decision_id = committed.body["data"]["decision_id"]
        .as_str()
        .expect("durable decision ID")
        .to_owned();

    let replay = request_json(
        &app,
        Method::POST,
        &endpoint,
        Some(waiver),
        Some(("decision-transport-0003", "http://127.0.0.1:4173")),
    )
    .await;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.body);
    assert_eq!(replay.body["data"]["decision_id"], decision_id);
    assert_eq!(replay.body["data"]["idempotent_replay"], true);

    let history = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/issues/{issue_id}/decisions?page_size=20"),
        None,
        None,
    )
    .await;
    assert_eq!(history.status, StatusCode::OK);
    validate_transport_schema(
        &history.body,
        "schemas/read/decision-history-page.schema.json",
    );
    assert_eq!(history.body["items"].as_array().expect("history").len(), 1);
    let result = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/decisions/{decision_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(result.status, StatusCode::OK);
    validate_transport_schema(&result.body, "schemas/read/decision-result.schema.json");
    assert_eq!(
        result.body["schema_version"],
        "migration-stream/read/decision-result/v1"
    );
    let counts = request_json(&app, Method::GET, "/api/v1/decision-counts", None, None).await;
    assert_eq!(counts.status, StatusCode::OK);
    validate_transport_schema(&counts.body, "schemas/read/decision-counts.schema.json");

    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("inspect bounded result counts");
    let attempts: i64 = connection
        .query_row("SELECT COUNT(*) FROM decision_attempts", [], |row| {
            row.get(0)
        })
        .expect("attempt count");
    let decisions: i64 = connection
        .query_row("SELECT COUNT(*) FROM issue_decisions", [], |row| row.get(0))
        .expect("decision count");
    assert_eq!(
        attempts, 1,
        "replay or rejected transport duplicated attempts"
    );
    assert_eq!(decisions, 1, "replay duplicated durable authority");
    drop(connection);

    let detail_after_waiver = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/issues/{issue_id}/decision-detail"),
        None,
        None,
    )
    .await;
    let data = &detail_after_waiver.body["data"];
    let accepted = json!({
        "schema_version": "migration-stream/write/accept-issue-as-is/v1",
        "validation_run_id": data["validation_run_id"],
        "issue_hash": data["issue_hash"],
        "practitioner_identifier": "synthetic-practitioner",
        "reason": "Retain the bounded synthetic effective value",
        "evidence_reference": null,
        "expected_active_decision_id": data["concurrency"]["expected_active_decision_id"],
        "expected_active_generation": data["concurrency"]["expected_active_generation"],
        "acknowledge_not_migration_result_approval": true,
        "expected_effective_value_hash": data["concurrency"]["expected_effective_value_hash"],
        "acknowledge_validation_truth_unchanged": true
    });
    let accepted = request_json(
        &app,
        Method::POST,
        &format!("/api/v1/issues/{issue_id}/decisions/accept-as-is"),
        Some(accepted),
        Some(("decision-transport-0004", "http://127.0.0.1:4173")),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);
    assert_eq!(accepted.body["data"]["disposition"], "ACCEPTED_AS_IS");

    let detail_after_acceptance = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/issues/{issue_id}/decision-detail"),
        None,
        None,
    )
    .await;
    let data = &detail_after_acceptance.body["data"];
    let exclusion = json!({
        "schema_version": "migration-stream/write/exclude-issue-row/v1",
        "validation_run_id": data["validation_run_id"],
        "issue_hash": data["issue_hash"],
        "practitioner_identifier": "synthetic-practitioner",
        "reason": "Bounded synthetic row-scope exclusion evidence",
        "evidence_reference": null,
        "expected_active_decision_id": data["concurrency"]["expected_active_decision_id"],
        "expected_active_generation": data["concurrency"]["expected_active_generation"],
        "acknowledge_not_migration_result_approval": true,
        "source_file": data["accepted_row"]["source_file"],
        "source_ordinal": data["accepted_row"]["source_ordinal"],
        "acknowledge_row_scope": true
    });
    let excluded = request_json(
        &app,
        Method::POST,
        &format!("/api/v1/issues/{issue_id}/decisions/exclude-row"),
        Some(exclusion),
        Some(("decision-transport-0005", "http://127.0.0.1:4173")),
    )
    .await;
    assert_eq!(excluded.status, StatusCode::OK, "{}", excluded.body);
    assert_eq!(excluded.body["data"]["disposition"], "EXCLUDED");

    let detail_after_exclusion = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/issues/{issue_id}/decision-detail"),
        None,
        None,
    )
    .await;
    let data = &detail_after_exclusion.body["data"];
    let correction = json!({
        "schema_version": "migration-stream/write/correct-issue/v1",
        "validation_run_id": data["validation_run_id"],
        "issue_hash": data["issue_hash"],
        "practitioner_identifier": "synthetic-practitioner",
        "reason": "Apply one bounded synthetic non-destructive overlay",
        "evidence_reference": null,
        "expected_active_decision_id": data["concurrency"]["expected_active_decision_id"],
        "expected_active_generation": data["concurrency"]["expected_active_generation"],
        "acknowledge_not_migration_result_approval": true,
        "correction": {
            "source_file": data["accepted_row"]["source_file"],
            "source_ordinal": data["accepted_row"]["source_ordinal"],
            "entity_id": data["entity_id"],
            "field_id": data["field_id"],
            "operation": "REPLACE_EFFECTIVE_TEXT",
            "value": "SYNTHETIC-CORRECTION",
            "expected_effective_value_hash": data["concurrency"]["expected_effective_value_hash"]
        }
    });
    let corrected = request_json(
        &app,
        Method::POST,
        &format!("/api/v1/issues/{issue_id}/decisions/correct"),
        Some(correction),
        Some(("decision-transport-0006", "http://127.0.0.1:4173")),
    )
    .await;
    assert_eq!(corrected.status, StatusCode::OK, "{}", corrected.body);
    assert_eq!(corrected.body["data"]["disposition"], "CORRECTED");
    assert_eq!(corrected.body["data"]["reevaluation_state"], "COMPLETE");
    let overlay_id = corrected.body["data"]["correction_overlay_id"]
        .as_str()
        .expect("effective overlay ID");
    let overlay = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/corrections/{overlay_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(overlay.status, StatusCode::OK, "{}", overlay.body);
    validate_transport_schema(
        &overlay.body,
        "schemas/read/correction-overlay-detail.schema.json",
    );
    assert_eq!(overlay.body["data"]["active"], true);

    let final_history = request_json(
        &app,
        Method::GET,
        &format!("/api/v1/issues/{issue_id}/decisions?page_size=20"),
        None,
        None,
    )
    .await;
    assert_eq!(final_history.status, StatusCode::OK);
    assert_eq!(
        final_history.body["items"]
            .as_array()
            .expect("complete history")
            .len(),
        4
    );
    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("inspect complete four-action history");
    let attempts: i64 = connection
        .query_row("SELECT COUNT(*) FROM decision_attempts", [], |row| {
            row.get(0)
        })
        .expect("final attempt count");
    let decisions: i64 = connection
        .query_row("SELECT COUNT(*) FROM issue_decisions", [], |row| row.get(0))
        .expect("final decision count");
    let effectivations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM correction_effectivations",
            [],
            |row| row.get(0),
        )
        .expect("final effectivation count");
    assert_eq!(attempts, 4);
    assert_eq!(decisions, 4);
    assert_eq!(effectivations, 1);
    drop(connection);

    for (index, action) in ["correct", "waive", "accept-as-is", "exclude-row"]
        .into_iter()
        .enumerate()
    {
        let response = request_json(
            &app,
            Method::POST,
            &format!("/api/v1/issues/{issue_id}/decisions/{action}"),
            Some(json!({})),
            Some((
                &format!("decision-invalid-route-{index:04}"),
                "http://127.0.0.1:4173",
            )),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST, "route {action}");
        assert_eq!(error_code(&response), "INVALID_DECISION_COMMAND");
    }
    assert_eq!(
        committed.headers["cache-control"], "no-store",
        "governed result must not be cached"
    );
    assert!(
        committed
            .headers
            .get("access-control-allow-origin")
            .is_none()
    );
}

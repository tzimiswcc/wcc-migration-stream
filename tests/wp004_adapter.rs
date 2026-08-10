use std::{
    fs,
    io::Write as _,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Method, Request, StatusCode},
};
use jsonschema::PatternOptions;
use migration_stream::{
    intake::ingest_package,
    mapping::validate_mapping,
    policy::IntakePolicy,
    profile_registry::{RegistryLimits, approve_profile, register_profile},
    profiling::{ProfilingPolicy, run_profile},
    validation::{ValidationPolicy, run_validation},
    workbench::{
        DEFAULT_LISTEN_ADDRESS, WorkbenchConfig, build_application_router, build_router,
        validate_loopback_address,
    },
};
use rusqlite::{Connection, params};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt as _;

const PROFILE_ID: &str = "synthetic.customer.readiness";
const PROFILE_VERSION: &str = "1.0.0";
const MAPPING_ID: &str = "synthetic.customer.valid";
const MAPPING_VERSION: &str = "1.0.0";

struct AdapterFixture {
    _temporary: TempDir,
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

fn prepare_fixture() -> AdapterFixture {
    let temporary = tempfile::tempdir_in(project_path("target"))
        .expect("link-safe temporary directory beneath target");
    let workspace = temporary.path().join("private-machine-workspace");
    fs::create_dir(&workspace).expect("create selected object workspace");
    migration_stream::object_workspace::establish_for_local_automation(
        &workspace,
        "Synthetic WP-004 workspace",
        "Customer readiness",
        "Automated adapter proof",
    )
    .expect("initialize object workspace");
    let intake = ingest_package(
        &project_path("fixtures/packages/valid.zip"),
        &workspace,
        &intake_policy(),
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
        "WP-004 adapter test",
        "Authorize this exact synthetic profile for local mapping and validation only",
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
    .expect("write bounded synthetic mapping");
    validate_mapping(
        &workspace,
        &intake.job_id,
        &project_path("schemas/source-mapping-version.schema.json"),
        &project_path("schemas/migration-object-profile.schema.json"),
        &mapping_path,
    )
    .expect("validate synthetic mapping");
    let profiling = run_profile(&workspace, &intake.job_id, &profiling_policy())
        .expect("profile accepted synthetic source");
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
    .expect("run bounded synthetic validation");
    AdapterFixture {
        _temporary: temporary,
        workspace,
        job_id: intake.job_id,
        profiling_run_id: profiling.run_id,
    }
}

fn router(fixture: &AdapterFixture) -> Router {
    build_router(WorkbenchConfig::new(
        &fixture.workspace,
        fixture.job_id.clone(),
        Some(fixture.profiling_run_id.clone()),
    ))
    .expect("preflight bounded adapter")
}

async fn request_json(app: &Router, method: Method, uri: &str) -> JsonResponse {
    request_json_with_body(app, method, uri, Body::empty(), None).await
}

async fn request_json_with_body(
    app: &Router,
    method: Method,
    uri: &str,
    body: Body,
    content_length: Option<usize>,
) -> JsonResponse {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:4173");
    if let Some(content_length) = content_length {
        request = request.header("content-length", content_length);
    }
    let request = request.body(body).expect("bounded test request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("infallible router response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 70 * 1024 * 1024)
        .await
        .expect("bounded response body");
    let body = serde_json::from_slice(&bytes).expect("JSON response");
    JsonResponse {
        status,
        headers,
        body,
    }
}

fn validate_transport_schema(document: &Value, relative_schema: &str) {
    let schema: Value = serde_json::from_slice(
        &fs::read(project_path(relative_schema)).expect("read transport schema"),
    )
    .expect("parse transport schema");
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .with_pattern_options(PatternOptions::regex())
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

fn assert_safe_error(response: &JsonResponse, expected_status: StatusCode, expected_code: &str) {
    assert_eq!(response.status, expected_status);
    validate_transport_schema(&response.body, "schemas/http/error-envelope.schema.json");
    assert_eq!(response.body["error"]["code"], expected_code);
    let serialized = response.body.to_string();
    assert!(!serialized.contains("state.sqlite3"));
    assert!(!serialized.contains("/Volumes/"));
    assert!(!serialized.contains("private-machine-workspace"));
    assert!(!serialized.contains("Acme, S.A."));
    assert!(!serialized.contains("leading zero 0007"));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn all_routes_preserve_contracts_bounds_and_read_only_storage() {
    let fixture = prepare_fixture();
    let database = fixture.workspace.join("state.sqlite3");
    let database_before = fs::read(&database).expect("snapshot database before reads");
    let app = router(&fixture);
    // SQLite may materialize an empty WAL namespace while opening an existing
    // WAL-mode database read-only. Capture the post-preflight namespace and
    // prove that HTTP requests neither change it nor authoritative DB bytes.
    let wal_before = fs::read(fixture.workspace.join("state.sqlite3-wal")).ok();
    let shm_before = fs::read(fixture.workspace.join("state.sqlite3-shm")).ok();

    let context = request_json(&app, Method::GET, "/api/v1/context").await;
    assert_eq!(context.status, StatusCode::OK);
    validate_transport_schema(&context.body, "schemas/http/workbench-context.schema.json");
    assert_eq!(context.body["read_only"], true);
    assert_eq!(
        context.body["workspace_label"],
        "Synthetic WP-004 workspace"
    );
    assert_eq!(
        context.body["supported_contracts"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    let context_text = context.body.to_string();
    assert!(!context_text.contains(&fixture.workspace.display().to_string()));
    assert!(!context_text.contains("state.sqlite3"));

    let routes = [
        ("/api/v1/job", "migration-stream/read/job-summary/v1"),
        (
            "/api/v1/files?page_size=1",
            "migration-stream/read/file-manifest/v1",
        ),
        (
            "/api/v1/profiling?page_size=1&include_samples=false",
            "migration-stream/read/profiling-summary/v1",
        ),
        ("/api/v1/profile", "migration-stream/read/profile-view/v1"),
        (
            "/api/v1/mapping?page_size=2",
            "migration-stream/read/mapping-coverage/v1",
        ),
        (
            "/api/v1/validation",
            "migration-stream/read/validation-summary/v1",
        ),
        (
            "/api/v1/issues?page_size=1&severity=ERROR",
            "migration-stream/read/issue-list/v1",
        ),
        (
            "/api/v1/reconciliation?page_size=1",
            "migration-stream/read/reconciliation-summary/v1",
        ),
        (
            "/api/v1/operations?page_size=1",
            "migration-stream/read/operational-cleanup/v1",
        ),
    ];
    for (uri, schema) in routes {
        let response = request_json(&app, Method::GET, uri).await;
        assert_eq!(response.status, StatusCode::OK, "route failed: {uri}");
        assert_eq!(response.body["schema_version"], schema, "route: {uri}");
        let broad = response.body.to_string();
        assert!(
            !broad.contains("000001"),
            "broad source value leaked: {uri}"
        );
        assert!(
            !broad.contains("Acme, S.A."),
            "broad source value leaked: {uri}"
        );
        assert!(
            !broad.contains("leading zero 0007"),
            "broad source value leaked: {uri}"
        );
    }

    let first_issues = request_json(&app, Method::GET, "/api/v1/issues?page_size=1").await;
    let cursor = first_issues.body["page"]["next_cursor"]
        .as_str()
        .expect("next server cursor");
    let second_uri = format!("/api/v1/issues?page_size=1&cursor={cursor}");
    let second_issues = request_json(&app, Method::GET, &second_uri).await;
    assert_eq!(second_issues.status, StatusCode::OK);
    assert_ne!(
        first_issues.body["items"][0],
        second_issues.body["items"][0]
    );
    let stale_uri = format!("/api/v1/issues?page_size=1&severity=ERROR&cursor={cursor}");
    let stale = request_json(&app, Method::GET, &stale_uri).await;
    assert_safe_error(&stale, StatusCode::BAD_REQUEST, "INVALID_REQUEST");

    let redacted = request_json(
        &app,
        Method::GET,
        "/api/v1/profiling?page_size=1&include_samples=false",
    )
    .await;
    assert!(redacted.body["items"][0].get("samples").is_none());
    let sampled = request_json(
        &app,
        Method::GET,
        "/api/v1/profiling?page_size=1&include_samples=true",
    )
    .await;
    assert_eq!(sampled.status, StatusCode::OK);
    assert!(sampled.body["items"][0]["samples"].is_array());

    let row = request_json(
        &app,
        Method::GET,
        "/api/v1/rows/1?source_file=customer_accounts.csv",
    )
    .await;
    assert_eq!(row.status, StatusCode::OK);
    assert_eq!(
        row.body["schema_version"],
        "migration-stream/read/row-detail/v1"
    );
    assert!(row.body.to_string().contains("000001"));
    assert!(row.body["data"]["source_values"].is_array());
    assert!(row.body["data"]["canonical_projection"].is_array());

    drop(app);
    assert_eq!(
        fs::read(&database).expect("snapshot database after reads"),
        database_before,
        "HTTP reads modified authoritative SQLite bytes"
    );
    assert_eq!(
        fs::read(fixture.workspace.join("state.sqlite3-wal")).ok(),
        wal_before,
        "HTTP reads created or changed the WAL"
    );
    assert_eq!(
        fs::read(fixture.workspace.join("state.sqlite3-shm")).ok(),
        shm_before,
        "HTTP reads created or changed SHM bytes"
    );
}

#[tokio::test]
async fn invalid_queries_methods_and_paths_use_safe_bounded_errors() {
    let fixture = prepare_fixture();
    let app = router(&fixture);
    for uri in [
        "/api/v1/files?page_size=0",
        "/api/v1/files?page_size=201",
        "/api/v1/files?workspace=/private/other",
        "/api/v1/context?workspace=/private/other",
        "/api/v1/job?job=another-job",
        "/api/v1/issues?source_ordinal=0",
        "/api/v1/issues?severity=CRITICAL",
        "/api/v1/issues?cursor=invalid",
        "/api/v1/operations?namespace=quarantine",
        "/api/v1/rows/0?source_file=customer_accounts.csv",
        "/api/v1/rows/1?source_file=attempt.part",
        "/api/v1/rows/1?source_file=internal.tombstone",
    ] {
        let response = request_json(&app, Method::GET, uri).await;
        assert_safe_error(&response, StatusCode::BAD_REQUEST, "INVALID_REQUEST");
    }

    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        let response = request_json(&app, method, "/api/v1/job").await;
        assert_safe_error(
            &response,
            StatusCode::METHOD_NOT_ALLOWED,
            "METHOD_NOT_ALLOWED",
        );
    }
    let body_response = request_json_with_body(
        &app,
        Method::GET,
        "/api/v1/job",
        Body::from("not permitted"),
        Some(13),
    )
    .await;
    assert_safe_error(
        &body_response,
        StatusCode::BAD_REQUEST,
        "REQUEST_BODY_NOT_ALLOWED",
    );
    let missing = request_json(&app, Method::GET, "/api/v1/raw-sql").await;
    assert_safe_error(&missing, StatusCode::NOT_FOUND, "API_ROUTE_NOT_FOUND");
    assert!(missing.headers.get("access-control-allow-origin").is_none());
    assert_eq!(
        missing.headers["content-security-policy"],
        "default-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'"
    );
    assert_eq!(missing.headers["x-content-type-options"], "nosniff");
}

#[tokio::test]
async fn schema_integrity_and_retained_identity_fail_closed_without_diagnostics() {
    let fixture = prepare_fixture();
    let app = router(&fixture);
    let database = fixture.workspace.join("state.sqlite3");
    let connection = Connection::open(&database).expect("open deliberate schema test mutation");
    connection
        .pragma_update(None, "user_version", 4)
        .expect("set unsupported schema");
    drop(connection);
    let schema = request_json(&app, Method::GET, "/api/v1/job").await;
    assert_safe_error(&schema, StatusCode::CONFLICT, "SCHEMA_UNSUPPORTED");

    let connection = Connection::open(&database).expect("restore deliberate schema mutation");
    connection
        .pragma_update(
            None,
            "user_version",
            migration_stream::state::SCHEMA_VERSION,
        )
        .expect("restore supported schema");
    connection
        .execute(
            "UPDATE jobs SET stored_archive = ?1 WHERE id = ?2",
            params!["packages/not-the-accepted-hash.zip", fixture.job_id],
        )
        .expect("create deliberate accepted-identity conflict");
    drop(connection);
    let integrity = request_json(&app, Method::GET, "/api/v1/job").await;
    assert_safe_error(&integrity, StatusCode::CONFLICT, "INTEGRITY_FAILURE");

    drop(app);
    let replacement_app = router(&prepare_fixture());
    drop(replacement_app);

    let fixture = prepare_fixture();
    let app = router(&fixture);
    let parked = fixture.workspace.with_extension("parked");
    #[cfg(unix)]
    {
        fs::rename(&fixture.workspace, &parked).expect("park startup-selected workspace");
        fs::create_dir(&fixture.workspace).expect("create replacement workspace directory");
        fs::copy(
            parked.join("state.sqlite3"),
            fixture.workspace.join("state.sqlite3"),
        )
        .expect("copy replacement database");
        let replaced = request_json(&app, Method::GET, "/api/v1/context").await;
        assert_safe_error(
            &replaced,
            StatusCode::CONFLICT,
            "WORKSPACE_IDENTITY_CHANGED",
        );
    }
    #[cfg(windows)]
    {
        let error = fs::rename(&fixture.workspace, &parked)
            .expect_err("retained Windows workspace handle must block name replacement");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(fixture.workspace.is_dir());
        assert!(!parked.exists());

        let intact = request_json(&app, Method::GET, "/api/v1/context").await;
        assert_eq!(intact.status, StatusCode::OK);
        assert_eq!(
            intact.body["schema_version"],
            "migration-stream/workbench/context/v1"
        );

        drop(app);
        fs::rename(&fixture.workspace, &parked)
            .expect("workspace rename must succeed after releasing retained handles");
        fs::rename(&parked, &fixture.workspace)
            .expect("restore workspace after retained-handle proof");
    }
}

#[tokio::test]
async fn same_origin_assets_never_swallow_api_or_method_errors() {
    let fixture = prepare_fixture();
    let assets = tempfile::tempdir_in(project_path("target")).expect("temporary asset root");
    fs::create_dir(assets.path().join("assets")).expect("asset directory");
    fs::write(
        assets.path().join("index.html"),
        "<!doctype html><title>Read-only workbench</title>",
    )
    .expect("index asset");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("index.html", assets.path().join("linked.js"))
            .expect("deliberate linked asset");
        assert!(
            build_application_router(
                WorkbenchConfig::new(
                    &fixture.workspace,
                    fixture.job_id.clone(),
                    Some(fixture.profiling_run_id.clone()),
                ),
                assets.path(),
            )
            .is_err(),
            "linked static assets must fail closed"
        );
        fs::remove_file(assets.path().join("linked.js")).expect("remove deliberate linked asset");
    }
    let mut script = fs::File::create(assets.path().join("assets/app.js")).expect("script asset");
    script
        .write_all(b"console.log('synthetic')")
        .expect("write script asset");
    let app = build_application_router(
        WorkbenchConfig::new(
            &fixture.workspace,
            fixture.job_id.clone(),
            Some(fixture.profiling_run_id.clone()),
        ),
        assets.path(),
    )
    .expect("build same-origin application router");

    let navigation = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/issues")
                .header("host", "127.0.0.1:4173")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(navigation.status(), StatusCode::OK);
    assert_eq!(
        navigation.headers()["content-type"],
        "text/html; charset=utf-8"
    );

    let missing_api = request_json(&app, Method::GET, "/api/v2/job").await;
    assert_safe_error(&missing_api, StatusCode::NOT_FOUND, "API_ROUTE_NOT_FOUND");
    let mutation = request_json(&app, Method::POST, "/issues").await;
    assert_safe_error(
        &mutation,
        StatusCode::METHOD_NOT_ALLOWED,
        "METHOD_NOT_ALLOWED",
    );
    let missing_asset = request_json(&app, Method::GET, "/assets/missing.js").await;
    assert_safe_error(&missing_asset, StatusCode::NOT_FOUND, "ASSET_NOT_FOUND");
}

#[test]
fn listener_and_startup_scope_are_loopback_and_fixed() {
    assert!(validate_loopback_address(DEFAULT_LISTEN_ADDRESS).is_ok());
    assert!(validate_loopback_address(SocketAddr::from(([0, 0, 0, 0], 4173))).is_err());
    assert!(validate_loopback_address(SocketAddr::from(([203, 0, 113, 9], 4173))).is_err());
}

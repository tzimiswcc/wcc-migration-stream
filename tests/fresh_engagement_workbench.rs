use std::{
    fs,
    io::{Cursor, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use fs2::FileExt as _;
use migration_stream::decision_read_model::DecisionReadService;
use migration_stream::decision_service::DecisionService;
use migration_stream::decisions::{
    DECISION_SCHEMA_VERSION, DecisionConcurrency, DecisionScope, IssueDecisionRequest,
    IssueDisposition,
};
use migration_stream::engagement_workbench::{
    AssistedRuntimeIdentityConfig, EngagementWorkbenchConfig, build_engagement_application_router,
    build_engagement_application_router_with_runtime_identity,
};
use migration_stream::publication::{PUBLICATION_COMMAND_SCHEMA, PublicationRequest};
use migration_stream::publication_read_model::PublicationReadService;
use migration_stream::publication_service::PublicationService;
use migration_stream::reconciliation_approval::{
    RECONCILIATION_SCHEMA_VERSION, ResultActionKind, ResultActionPayload,
};
use migration_stream::reconciliation_service::{
    CalculateReconciliationRequest, CreateCandidateRequest, ReconciliationApprovalService,
};
use migration_stream::{KernelError, state::recover_workspace};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use tokio::sync::Barrier;
use tower::ServiceExt as _;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const HOST: &str = "127.0.0.1:4173";
const ORIGIN: &str = "http://127.0.0.1:4173";

struct Fixture {
    _temporary: TempDir,
    workspace: PathBuf,
    assets: PathBuf,
    object_packs: PathBuf,
}

struct JsonResponse {
    status: StatusCode,
    body: Value,
}

struct BaselineRemediationProof {
    history: Value,
    archives: Vec<(String, Vec<u8>)>,
    keys_only_command: Value,
    keys_only_pack: Value,
}

fn project_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn make_owner_writable(path: &Path) {
    let mut permissions = fs::metadata(path).expect("file metadata").permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(not(unix))]
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "this non-Unix test helper clears only the platform read-only attribute"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).expect("make file owner-writable");
}

fn assert_global_integrity(workspace: &Path, label: &str) {
    let report = migration_stream::state::integrity_check(workspace).expect(label);
    assert!(report.ok, "{:?}", report.issues);
}

#[allow(clippy::type_complexity)]
fn validation_issue_scopes(
    workspace: &Path,
    job_id: &str,
    validation_run_id: &str,
) -> Vec<DecisionScope> {
    let connection = Connection::open(workspace.join("state.sqlite3"))
        .expect("open exact validation issue authority");
    let mut statement = connection
        .prepare(
            "SELECT issue.issue_id, issue.issue_hash, issue.source_file,
                    issue.source_ordinal, issue.line_start, issue.line_end,
                    issue.entity_id, issue.field_id,
                    run.profile_id, run.profile_version, run.profile_hash,
                    run.mapping_id, run.mapping_version, run.mapping_hash
             FROM validation_issues AS issue
             JOIN validation_runs AS run ON run.id = issue.run_id
             WHERE issue.run_id = ?1
             ORDER BY issue.issue_id",
        )
        .expect("prepare exact validation issue scopes");
    statement
        .query_map([validation_run_id], |row| {
            Ok(DecisionScope {
                job_id: job_id.to_owned(),
                validation_run_id: validation_run_id.to_owned(),
                issue_id: row.get(0)?,
                issue_hash: row.get(1)?,
                accepted_source_file: row.get(2)?,
                source_ordinal: u64::try_from(row.get::<_, i64>(3)?).unwrap_or_default(),
                line_start: u64::try_from(row.get::<_, i64>(4)?).unwrap_or_default(),
                line_end: u64::try_from(row.get::<_, i64>(5)?).unwrap_or_default(),
                canonical_entity_id: row.get(6)?,
                canonical_field_id: row.get(7)?,
                profile_id: row.get(8)?,
                profile_version: row.get(9)?,
                profile_hash: row.get(10)?,
                mapping_id: row.get(11)?,
                mapping_version: row.get(12)?,
                mapping_hash: row.get(13)?,
            })
        })
        .expect("query exact validation issue scopes")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect exact validation issue scopes")
}

fn close_validation_issues_for_local_readiness(
    workspace: &Path,
    job_id: &str,
    validation_run_id: &str,
) {
    let reader = DecisionReadService::new(workspace);
    let writer = DecisionService::new(
        workspace,
        project_path("schemas/migration-object-profile.schema.json"),
    );
    let scopes = validation_issue_scopes(workspace, job_id, validation_run_id);
    assert!(
        !scopes.is_empty(),
        "readiness proof requires validation issues"
    );
    for (index, scope) in scopes.into_iter().enumerate() {
        let detail = reader
            .issue_detail(job_id, validation_run_id, &scope.issue_id)
            .expect("read exact issue concurrency");
        writer
            .apply(&IssueDecisionRequest {
                schema_version: DECISION_SCHEMA_VERSION,
                idempotency_key: format!("fresh-close-issue-{index:04}"),
                disposition: IssueDisposition::AcceptedAsIs,
                scope,
                concurrency: DecisionConcurrency {
                    expected_active_decision_id: detail
                        .data
                        .concurrency
                        .expected_active_decision_id,
                    expected_active_decision_generation: detail
                        .data
                        .concurrency
                        .expected_active_generation,
                    expected_effective_value_hash: Some(
                        detail.data.concurrency.expected_effective_value_hash,
                    ),
                },
                practitioner_id: "Synthetic revision readiness practitioner".to_owned(),
                reason: "Close this exact synthetic issue for immutable prior-run package proof."
                    .to_owned(),
                evidence_reference: Some("SPINE-F-SYNTHETIC-LOCAL-ONLY".to_owned()),
                authority_separation_acknowledged: true,
                row_scope_exclusion_acknowledged: false,
                correction_overlay: None,
            })
            .expect("close exact synthetic validation issue");
    }
}

fn immutable_downstream_evidence(workspace: &Path, job_id: &str) -> Vec<(String, Vec<String>)> {
    let connection = Connection::open(workspace.join("state.sqlite3"))
        .expect("open immutable downstream evidence");
    let queries = [
        (
            "issue decisions",
            "SELECT json_array(decision_id, canonical_decision_json, decision_hash)
             FROM issue_decisions WHERE job_id = ?1 ORDER BY decision_id",
        ),
        (
            "correction overlays",
            "SELECT json_array(overlay.overlay_id, overlay.canonical_overlay_json,
                               overlay.overlay_hash)
             FROM correction_overlays AS overlay
             JOIN issue_decisions AS decision
               ON decision.correction_overlay_id = overlay.overlay_id
             WHERE decision.job_id = ?1 ORDER BY overlay.overlay_id",
        ),
        (
            "correction effectivations",
            "SELECT json_array(effect.effectivation_id, effect.effective_lineage_json,
                               effect.effectivation_hash)
             FROM correction_effectivations AS effect
             JOIN issue_decisions AS decision ON decision.decision_id = effect.decision_id
             WHERE decision.job_id = ?1 ORDER BY effect.effectivation_id",
        ),
        (
            "reconciliation snapshots",
            "SELECT json_array(snapshot_id, governing_input_hash, row_set_hash, snapshot_hash)
             FROM reconciliation_snapshots WHERE job_id = ?1 ORDER BY snapshot_id",
        ),
        (
            "result candidates",
            "SELECT json_array(candidate_id, canonical_candidate_json, candidate_hash)
             FROM migration_result_candidates WHERE job_id = ?1 ORDER BY candidate_id",
        ),
        (
            "result actions",
            "SELECT json_array(action_id, canonical_action_json, action_hash)
             FROM migration_result_actions WHERE job_id = ?1 ORDER BY action_id",
        ),
        (
            "readiness packages",
            "SELECT json_array(publication_id, canonical_manifest_json,
                               semantic_publication_hash, physical_archive_digest)
             FROM publication_records WHERE job_id = ?1 ORDER BY publication_id",
        ),
    ];
    queries
        .into_iter()
        .map(|(label, sql)| {
            let mut statement = connection
                .prepare(sql)
                .expect("prepare immutable downstream evidence query");
            let rows = statement
                .query_map([job_id], |row| row.get::<_, String>(0))
                .expect("query immutable downstream evidence")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("collect immutable downstream evidence");
            (label.to_owned(), rows)
        })
        .collect()
}

fn revised_launch_authority_snapshot(workspace: &Path) -> Value {
    let connection = Connection::open(workspace.join("state.sqlite3"))
        .expect("open revised-launch authority snapshot");
    let active_context: String = connection
        .query_row(
            "SELECT json_array(
                 engagement_id, generation, current_preflight_id, active_job_id,
                 active_profiling_run_id, selected_object_pack_id,
                 destination_origin, destination_authority_id,
                 destination_origin_hash,
                 selected_profile_id, selected_profile_version,
                 selected_profile_hash, profile_stage, active_mapping_id,
                 active_mapping_version, active_mapping_hash, mapping_stage,
                 active_validation_run_id, validation_stage, updated_at
             ) FROM engagement_active_context",
            [],
            |row| row.get(0),
        )
        .expect("read full active engagement authority");
    let tour_head: String = connection
        .query_row(
            "SELECT json_array(
                 engagement_id, generation, active_tour_id,
                 pending_tour_attempt_id, updated_at
             ) FROM engagement_tour_heads",
            [],
            |row| row.get(0),
        )
        .expect("read full revised-tour head");
    let count = |table: &str| -> i64 {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count revised-launch authority rows")
    };
    json!({
        "activeContext": serde_json::from_str::<Value>(&active_context)
            .expect("parse active engagement authority"),
        "tourHead": serde_json::from_str::<Value>(&tour_head)
            .expect("parse revised-tour head"),
        "registeredTours": count("engagement_tours"),
        "comparisons": count("engagement_validation_comparisons"),
        "findingSets": count("engagement_tour_finding_sets"),
        "validationRuns": count("validation_runs"),
        "tourCommands": count("engagement_tour_command_attempts")
    })
}

fn replace_active_mapping_hash(workspace: &Path, replacement: &str) -> String {
    assert_eq!(replacement.len(), 64);
    let connection =
        Connection::open(workspace.join("state.sqlite3")).expect("open active mapping authority");
    let original: String = connection
        .query_row(
            "SELECT active_mapping_hash FROM engagement_active_context",
            [],
            |row| row.get(0),
        )
        .expect("read active mapping hash");
    let changed = connection
        .execute(
            "UPDATE engagement_active_context SET active_mapping_hash = ?1",
            [replacement],
        )
        .expect("replace active mapping hash for bounded tamper proof");
    assert_eq!(changed, 1);
    original
}

fn assert_history_contains_exact(actual: &Value, expected: &Value) {
    let actual_items = actual["items"]
        .as_array()
        .expect("actual remediation history items");
    for expected_item in expected["items"]
        .as_array()
        .expect("expected remediation history items")
    {
        assert!(
            actual_items.iter().any(|item| item == expected_item),
            "remediation history rewrote or omitted prior immutable evidence: {expected_item:?}"
        );
    }
}

#[cfg(unix)]
fn assert_unproven_retained_identity_not_blessed(connection: &Connection, preflight_id: &str) {
    let publication_intent: String = connection
        .query_row(
            "SELECT detail FROM engagement_preflight_events
             WHERE preflight_id = ?1
               AND detail LIKE '%preflight-publish-intent/v2%'
             ORDER BY sequence DESC LIMIT 1",
            [preflight_id],
            |row| row.get(0),
        )
        .expect("load failed-closed publication authority");
    let publication_intent: Value =
        serde_json::from_str(&publication_intent).expect("parse publication authority");
    assert_eq!(
        publication_intent["retainedFileIdentity"], publication_intent["partialFileIdentity"],
        "an unproven retained name must never be blessed as deduplicated authority"
    );
}

fn fixture_with_pack(name: &str, pack_id: &str, profile_file: &str) -> Fixture {
    let temporary = tempfile::tempdir_in(project_path("target"))
        .expect("create link-safe setup fixture beneath target");
    let workspace = temporary.path().join(name);
    let assets = temporary.path().join("assets");
    let object_packs = temporary.path().join("object-packs");
    let pack_version = object_packs.join(pack_id).join("1.0.0");
    fs::create_dir_all(&workspace).expect("create startup-selected workspace");
    fs::create_dir_all(&assets).expect("create asset directory");
    fs::create_dir_all(&pack_version).expect("create controlled pack directory");
    fs::write(
        assets.join("index.html"),
        "<!doctype html><title>Fresh engagement test</title>",
    )
    .expect("write bounded test asset");
    for file_name in ["manifest.json", profile_file] {
        fs::copy(
            project_path(&format!("object-packs/{pack_id}/1.0.0/{file_name}")),
            pack_version.join(file_name),
        )
        .expect("copy controlled pack fixture");
    }
    Fixture {
        _temporary: temporary,
        workspace,
        assets,
        object_packs,
    }
}

fn fixture(name: &str) -> Fixture {
    fixture_with_pack(
        name,
        "synthetic.customer.readiness",
        "synthetic-profile.reviewed.json",
    )
}

fn sales_fixture(name: &str) -> Fixture {
    fixture_with_pack(
        name,
        "synthetic.sales-document.readiness",
        "synthetic-profile.reviewed.json",
    )
}

fn application(fixture: &Fixture) -> Router {
    migration_stream::object_workspace::establish_for_local_automation(
        &fixture.workspace,
        "Synthetic browser workspace",
        "Migration readiness proof",
        "Automated browser integration proof",
    )
    .expect("establish object workspace");
    application_without_establishing_workspace(fixture)
}

fn application_without_establishing_workspace(fixture: &Fixture) -> Router {
    build_engagement_application_router(
        EngagementWorkbenchConfig {
            workspace: fixture.workspace.clone(),
            intake_policy: project_path("config/engagement-intake-policy.json"),
            profiling_policy: project_path("config/engagement-profiling-policy.json"),
            validation_policy: project_path("config/engagement-validation-policy.json"),
            object_packs: fixture.object_packs.clone(),
            profile_schema: project_path("schemas/migration-object-profile.schema.json"),
            mapping_schema: project_path("schemas/source-mapping-version.schema.json"),
        },
        &fixture.assets,
    )
    .expect("build fresh engagement workbench")
}

fn assisted_runtime_application(fixture: &Fixture, session_token: &str) -> Router {
    assisted_runtime_application_at(fixture, session_token, HOST)
}

fn assisted_runtime_application_at(
    fixture: &Fixture,
    session_token: &str,
    listen_address: &str,
) -> Router {
    build_engagement_application_router_with_runtime_identity(
        EngagementWorkbenchConfig {
            workspace: fixture.workspace.clone(),
            intake_policy: project_path("config/engagement-intake-policy.json"),
            profiling_policy: project_path("config/engagement-profiling-policy.json"),
            validation_policy: project_path("config/engagement-validation-policy.json"),
            object_packs: fixture.object_packs.clone(),
            profile_schema: project_path("schemas/migration-object-profile.schema.json"),
            mapping_schema: project_path("schemas/source-mapping-version.schema.json"),
        },
        &fixture.assets,
        Some(&AssistedRuntimeIdentityConfig {
            session_token: session_token.to_owned(),
            listen_address: listen_address.to_owned(),
        }),
    )
    .expect("build assisted runtime engagement workbench")
}

fn source_package(csv: &str) -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    writer
        .start_file(
            "customer_accounts.csv",
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .expect("start synthetic source");
    writer
        .write_all(csv.as_bytes())
        .expect("write synthetic source");
    writer.finish().expect("finish synthetic ZIP").into_inner()
}

fn remediation_zip_member(archive_bytes: &[u8], member_name: &str) -> Vec<u8> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(archive_bytes)).expect("open verified remediation ZIP");
    let mut member = archive
        .by_name(member_name)
        .expect("read expected remediation member");
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut member, &mut bytes).expect("read bounded remediation member");
    bytes
}

fn sales_source_package() -> Vec<u8> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    for source_file in ["deal_book.csv", "deal_detail.csv"] {
        writer
            .start_file(
                source_file,
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .expect("start unfamiliar sales source");
        writer
            .write_all(
                &fs::read(project_path(&format!(
                    "fixtures/sales-document-unfamiliar/{source_file}"
                )))
                .expect("read unfamiliar sales source"),
            )
            .expect("write unfamiliar sales source");
    }
    writer.finish().expect("finish sales ZIP").into_inner()
}

fn sales_mapping_snapshot() -> Value {
    let binding = |binding_id: &str,
                   entity_id: &str,
                   source_file: &str,
                   source_column: &str,
                   source_column_ordinal: usize,
                   target_field_id: &str,
                   kind: &str,
                   lookup_table_id: Option<&str>| {
        let mut value = json!({
            "bindingId": binding_id,
            "targetEntityId": entity_id,
            "sourceFile": source_file,
            "sourceColumn": source_column,
            "sourceColumnOrdinal": source_column_ordinal,
            "targetFieldId": target_field_id,
            "kind": kind
        });
        if let Some(table_id) = lookup_table_id {
            value["lookupTableId"] = json!(table_id);
        }
        value
    };
    json!({
        "sourceEntityAssignments": [
            {"sourceFile": "deal_book.csv", "entityId": "SalesDocument"},
            {"sourceFile": "deal_detail.csv", "entityId": "SalesDocumentItem"}
        ],
        "bindings": [
            binding("header.source-document-id", "SalesDocument", "deal_book.csv", "case_ref", 0, "sourceDocumentId", "direct", None),
            binding("header.customer-id", "SalesDocument", "deal_book.csv", "party_ref", 1, "customerId", "trim", None),
            binding("header.document-date", "SalesDocument", "deal_book.csv", "booked_on", 2, "documentDate", "direct", None),
            binding("header.document-type", "SalesDocument", "deal_book.csv", "route_code", 3, "documentType", "lookup", Some("sales-document-route-map")),
            binding("header.currency", "SalesDocument", "deal_book.csv", "money_iso", 4, "currency", "uppercase_ascii", None),
            binding("header.customer-reference", "SalesDocument", "deal_book.csv", "buy_ref", 5, "customerReference", "empty_as_null", None),
            binding("header.selling-unit", "SalesDocument", "deal_book.csv", "owning_desk", 6, "sellingUnit", "uppercase_ascii", None),
            binding("item.parent-document-id", "SalesDocumentItem", "deal_detail.csv", "case_link", 0, "parentDocumentId", "direct", None),
            binding("item.source-item-id", "SalesDocumentItem", "deal_detail.csv", "seq_ref", 1, "sourceItemId", "direct", None),
            binding("item.product-id", "SalesDocumentItem", "deal_detail.csv", "offer_ref", 2, "productId", "trim", None),
            binding("item.quantity", "SalesDocumentItem", "deal_detail.csv", "demand_qty", 3, "quantity", "direct", None),
            binding("item.unit-of-measure", "SalesDocumentItem", "deal_detail.csv", "measure", 4, "unitOfMeasure", "uppercase_ascii", None),
            binding("item.unit-price", "SalesDocumentItem", "deal_detail.csv", "net_rate", 5, "unitPrice", "empty_as_null", None),
            binding("item.requested-date", "SalesDocumentItem", "deal_detail.csv", "need_by", 6, "requestedDate", "empty_as_null", None)
        ],
        "relationshipBindings": [{
            "relationshipId": "sales-document.items",
            "fromEntityId": "SalesDocument",
            "toEntityId": "SalesDocumentItem",
            "cardinality": "1..*",
            "keyPairs": [{
                "fromFieldId": "sourceDocumentId",
                "toFieldId": "parentDocumentId"
            }]
        }],
        "referenceTables": [
            {
                "tableId": "sales-document-type-domain",
                "version": "1.0.0",
                "entityId": "SalesDocument",
                "values": {"RETURN": "RETURN", "STANDARD": "STANDARD"}
            },
            {
                "tableId": "sales-document-route-map",
                "version": "1.0.0",
                "entityId": "SalesDocument",
                "values": {"N": "STANDARD", "R": "RETURN"}
            }
        ],
        "author": "Synthetic local test practitioner"
    })
}

#[test]
fn controlled_object_packs_match_their_checked_in_manifest_schema() {
    let schema_path = project_path("schemas/engagement/object-pack-manifest.schema.json");
    let schema: Value =
        serde_json::from_slice(&fs::read(&schema_path).expect("read manifest schema"))
            .expect("parse manifest schema");
    jsonschema::draft202012::meta::validate(&schema).expect("valid manifest meta-schema");
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .build(&schema)
        .expect("compile controlled manifest schema");
    for pack_id in [
        "synthetic.customer.readiness",
        "synthetic.sales-document.readiness",
    ] {
        let manifest_path = project_path(&format!("object-packs/{pack_id}/1.0.0/manifest.json"));
        let manifest: Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("read controlled manifest"))
                .expect("parse controlled manifest");
        let errors = validator
            .iter_errors(&manifest)
            .map(|error| format!("{}: {error}", error.instance_path()))
            .collect::<Vec<_>>();
        assert!(errors.is_empty(), "{pack_id}: {errors:?}");
    }
}

async fn get_json(app: &Router, uri: &str) -> JsonResponse {
    request(app, Method::GET, uri, Body::empty(), None, None).await
}

async fn get_bytes(app: &Router, uri: &str) -> (StatusCode, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header("host", HOST)
                .body(Body::empty())
                .expect("build bounded byte request"),
        )
        .await
        .expect("infallible setup router");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 32 * 1024 * 1024)
        .await
        .expect("read bounded byte response")
        .to_vec();
    (status, bytes)
}

async fn command_json(
    app: &Router,
    method: Method,
    uri: &str,
    intent: &str,
    idempotency_key: &str,
    body: Value,
) -> JsonResponse {
    request(
        app,
        method,
        uri,
        Body::from(serde_json::to_vec(&body).expect("serialize command")),
        Some(("application/json", intent)),
        Some(idempotency_key),
    )
    .await
}

async fn upload_zip(
    app: &Router,
    preflight_id: &str,
    idempotency_key: &str,
    bytes: Vec<u8>,
) -> JsonResponse {
    request(
        app,
        Method::PUT,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/content"),
        Body::from(bytes),
        Some(("application/zip", "engagement-source-preflight-content-v1")),
        Some(idempotency_key),
    )
    .await
}

async fn create_active_remediation_pack(
    app: &Router,
    prefix: &str,
    include_offending_values: bool,
) -> (Value, Value, Vec<u8>) {
    let options = get_json(app, "/api/v1/engagement/remediation-packs/options").await;
    assert_eq!(options.status, StatusCode::OK, "{:?}", options.body);
    let files = options.body["files"]
        .as_array()
        .expect("active remediation source-file options");
    let (file, key_column) = files
        .iter()
        .find_map(|file| {
            file["columns"]
                .as_array()
                .and_then(|columns| {
                    columns.iter().find(|column| {
                        column["sourceColumn"] == "migrationPartnerId"
                            && column["suggested"] == true
                    })
                })
                .map(|column| (file, column))
        })
        .expect("active revision retains the governed mapped-key suggestion");
    let selection = json!({
        "entityId": file["entityId"],
        "sourceFile": file["sourceFile"],
        "sourceColumnOrdinal": key_column["sourceColumnOrdinal"],
        "sourceColumn": key_column["sourceColumn"]
    });
    let mode = if include_offending_values {
        "offending-values"
    } else {
        "keys-only"
    };
    let idempotency_key = format!("{prefix}-{mode}-0001");
    let mut command = json!({
        "schemaVersion": "migration-stream/engagement/write/create-remediation-pack/v1",
        "expectedEngagementGeneration": options.body["engagementGeneration"],
        "expectedValidationRunId": options.body["validationRunId"],
        "expectedOptionsHash": options.body["optionsHash"],
        "selectedKeyColumns": [selection],
        "actor": "Synthetic revision remediation practitioner",
        "reason": format!("Create the exact active revision {mode} local remediation pack."),
        "localDataExportAcknowledgement": "migration-stream/local-remediation-data-export/v1",
        "includeOffendingValues": include_offending_values,
        "idempotencyKey": idempotency_key
    });
    if include_offending_values {
        command["offendingValuesAcknowledgement"] =
            json!("migration-stream/local-remediation-offending-values-export/v1");
    }
    let created = command_json(
        app,
        Method::POST,
        "/api/v1/engagement/remediation-packs",
        "engagement-remediation-pack-create-v1",
        &idempotency_key,
        command,
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);
    assert_eq!(created.body["reusedAttempt"], false);
    assert_eq!(
        created.body["pack"]["includeOffendingValues"],
        include_offending_values
    );
    assert_eq!(
        created.body["pack"]["validationRunId"],
        options.body["validationRunId"]
    );
    for broad in [&options.body, &created.body] {
        let broad = broad.to_string();
        for received in [
            "CUST-001",
            "CUST-002",
            "CUST-003",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(
                !broad.contains(received),
                "active remediation response leaked received value {received}"
            );
        }
    }
    let pack = created.body["pack"].clone();
    let pack_id = pack["packId"].as_str().expect("active remediation pack ID");
    let (status, bytes) = get_bytes(
        app,
        &format!("/api/v1/engagement/remediation-packs/{pack_id}/download"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        migration_stream::canonical::lower_hex(Sha256::digest(&bytes)),
        pack["archiveSha256"]
    );
    (options.body, pack, bytes)
}

async fn request(
    app: &Router,
    method: Method,
    uri: &str,
    body: Body,
    command: Option<(&str, &str)>,
    idempotency_key: Option<&str>,
) -> JsonResponse {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", HOST);
    if let Some((content_type, intent)) = command {
        builder = builder
            .header("origin", ORIGIN)
            .header("content-type", content_type)
            .header("accept", "application/json")
            .header("x-migration-stream-intent", intent)
            .header(
                "idempotency-key",
                idempotency_key.expect("command identity"),
            );
    }
    let response = app
        .clone()
        .oneshot(builder.body(body).expect("build request"))
        .await
        .expect("infallible setup router");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 32 * 1024 * 1024)
        .await
        .expect("read bounded response");
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "response must be JSON ({error}); bytes={:?}",
            String::from_utf8_lossy(&bytes)
        )
    });
    JsonResponse { status, body }
}

#[allow(clippy::too_many_lines)]
async fn complete_source_revision_http(
    app: &Router,
    prefix: &str,
    csv: &str,
    tamper_active_mapping_before_launch: Option<&Path>,
) -> (Value, Value) {
    let before = get_json(app, "/api/v1/engagement/tours").await;
    assert_eq!(before.status, StatusCode::OK, "{:?}", before.body);
    let expected_start_generation = if before.body["activeTour"].is_null() {
        let engagement = get_json(app, "/api/v1/engagement").await;
        assert_eq!(engagement.status, StatusCode::OK, "{:?}", engagement.body);
        engagement.body["state"]["generation"].clone()
    } else {
        before.body["engagementGeneration"].clone()
    };
    assert!(
        expected_start_generation.is_u64(),
        "tour start requires a numeric generation; chain={:?}",
        before.body
    );
    let start_key = format!("{prefix}-start");
    let start_body = json!({
        "schemaVersion": "migration-stream/engagement/write/start-revised-tour/v1",
        "idempotencyKey": start_key,
        "expectedEngagementGeneration": expected_start_generation,
        "actor": "Synthetic revision test practitioner",
        "reason": "Start the next exact immutable synthetic source revision.",
        "acknowledgement": "migration-stream/revised-source-compared-validation/v1"
    });
    let started = command_json(
        app,
        Method::POST,
        "/api/v1/engagement/tours/start",
        "engagement-revised-tour-start-v1",
        &start_key,
        start_body.clone(),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{:?}", started.body);
    let tour_attempt_id = started.body["pendingAttempt"]["tourAttemptId"]
        .as_str()
        .expect("started revision attempt ID")
        .to_owned();
    let pending_journey = get_json(app, "/api/v1/journey").await;
    assert_eq!(
        pending_journey.status,
        StatusCode::OK,
        "{:?}",
        pending_journey.body
    );
    assert_eq!(pending_journey.body["currentStageId"], "COMPARE");
    assert_eq!(pending_journey.body["stages"][5]["status"], "CURRENT");
    assert_eq!(pending_journey.body["stages"][5]["href"], "/comparison");
    assert_eq!(pending_journey.body["stages"][7]["status"], "AVAILABLE");
    assert_eq!(
        pending_journey.body["stages"]
            .as_array()
            .expect("pending journey stages")
            .iter()
            .filter(|stage| stage["status"] == "CURRENT")
            .count(),
        1
    );

    let reserve_key = format!("{prefix}-reserve");
    let reserved = command_json(
        app,
        Method::POST,
        "/api/v1/engagement/tours/revised-source-preflights",
        "engagement-revised-source-reserve-v1",
        &reserve_key,
        json!({
            "schemaVersion": "migration-stream/engagement/write/reserve-revised-source-preflight/v1",
            "idempotencyKey": reserve_key,
            "expectedEngagementGeneration": started.body["engagementGeneration"],
            "tourAttemptId": tour_attempt_id,
            "sourceName": format!("{prefix}.zip")
        }),
    )
    .await;
    assert_eq!(reserved.status, StatusCode::OK, "{:?}", reserved.body);
    let preflight_id = reserved.body["preflight"]["preflightId"]
        .as_str()
        .expect("reserved revised preflight ID")
        .to_owned();
    let uploaded = upload_zip(app, &preflight_id, &reserve_key, source_package(csv)).await;
    assert_eq!(uploaded.status, StatusCode::OK, "{:?}", uploaded.body);
    assert_eq!(uploaded.body["state"], "READY");

    let ready = get_json(app, "/api/v1/engagement/tours").await;
    assert_eq!(ready.status, StatusCode::OK, "{:?}", ready.body);
    assert_eq!(ready.body["pendingAttempt"]["state"], "PREFLIGHT_READY");
    assert_eq!(ready.body["pendingAttempt"]["preflightId"], preflight_id);
    let accept_key = format!("{prefix}-accept");
    let accepted = command_json(
        app,
        Method::POST,
        &format!("/api/v1/engagement/tours/revised-source-preflights/{preflight_id}/accept"),
        "engagement-revised-source-accept-v1",
        &accept_key,
        json!({
            "schemaVersion": "migration-stream/engagement/write/accept-revised-source/v1",
            "idempotencyKey": accept_key,
            "expectedEngagementGeneration": ready.body["engagementGeneration"],
            "tourAttemptId": tour_attempt_id,
            "preflightId": preflight_id,
            "expectedSourceHash": ready.body["pendingAttempt"]["sourceHash"],
            "expectedManifestHash": ready.body["pendingAttempt"]["manifestHash"],
            "actor": "Synthetic revision test practitioner",
            "reason": "Accept the exact same-structure synthetic revision locally.",
            "acknowledgement": "migration-stream/revised-source-compared-validation/v1"
        }),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{:?}", accepted.body);
    assert_eq!(
        accepted.body["reusedProfile"], false,
        "each immutable source revision must receive a fresh sample-free profiling run"
    );
    assert_eq!(
        accepted.body["reuseAuthority"]["profileId"],
        ready.body["activeTour"]["profileId"]
    );
    assert_eq!(
        accepted.body["reuseAuthority"]["profileHash"],
        ready.body["activeTour"]["profileHash"]
    );

    let launch_key = format!("{prefix}-launch");
    let launch_body = json!({
        "schemaVersion": "migration-stream/engagement/write/launch-revised-validation/v1",
        "idempotencyKey": launch_key,
        "expectedEngagementGeneration": accepted.body["engagementGeneration"],
        "tourAttemptId": tour_attempt_id,
        "expectedReuseAuthorityId": accepted.body["reuseAuthority"]["reuseAuthorityId"],
        "expectedReuseAuthorityHash": accepted.body["reuseAuthority"]["reuseAuthorityHash"],
        "actor": "Synthetic revision test practitioner",
        "reason": "Validate and compare this exact immutable source revision.",
        "acknowledgement": "migration-stream/revised-source-compared-validation/v1"
    });
    if let Some(workspace) = tamper_active_mapping_before_launch {
        let original_snapshot = revised_launch_authority_snapshot(workspace);
        let mut tampered_hash = "f".repeat(64);
        let original_hash = replace_active_mapping_hash(workspace, &tampered_hash);
        if original_hash == tampered_hash {
            tampered_hash = "e".repeat(64);
            assert_eq!(
                replace_active_mapping_hash(workspace, &tampered_hash),
                original_hash
            );
        }
        let tampered_snapshot = revised_launch_authority_snapshot(workspace);
        assert_ne!(tampered_snapshot, original_snapshot);
        let rejected = command_json(
            app,
            Method::POST,
            "/api/v1/engagement/tours/validation/launch",
            "engagement-revised-validation-launch-v1",
            &launch_key,
            launch_body.clone(),
        )
        .await;
        assert_eq!(rejected.status, StatusCode::CONFLICT, "{:?}", rejected.body);
        assert_eq!(
            revised_launch_authority_snapshot(workspace),
            tampered_snapshot,
            "a non-run active mapping tamper created candidate validation/tour/comparison evidence or advanced authority"
        );
        assert_eq!(
            replace_active_mapping_hash(workspace, &original_hash),
            tampered_hash
        );
        assert_eq!(
            revised_launch_authority_snapshot(workspace),
            original_snapshot,
            "restoring the exact active mapping hash did not recover the pre-launch authority"
        );
    }
    let launched = command_json(
        app,
        Method::POST,
        "/api/v1/engagement/tours/validation/launch",
        "engagement-revised-validation-launch-v1",
        &launch_key,
        launch_body,
    )
    .await;
    assert_eq!(launched.status, StatusCode::OK, "{:?}", launched.body);
    let chain = get_json(app, "/api/v1/engagement/tours").await;
    assert_eq!(chain.status, StatusCode::OK, "{:?}", chain.body);
    assert!(chain.body["pendingAttempt"].is_null());
    assert_eq!(
        chain.body["activeTour"]["tourNumber"],
        launched.body["activeTour"]["tourNumber"]
    );
    (launched.body, chain.body)
}

#[allow(clippy::too_many_lines)]
async fn exercise_remediation_pack_http(
    app: &Router,
    fixture: &Fixture,
) -> BaselineRemediationProof {
    let options = get_json(app, "/api/v1/engagement/remediation-packs/options").await;
    assert_eq!(options.status, StatusCode::OK, "{:?}", options.body);
    assert_eq!(
        options.body["schemaVersion"],
        "migration-stream/engagement/remediation-pack-options/v1"
    );
    assert!(
        options.body["issueCount"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    let files = options.body["files"]
        .as_array()
        .expect("remediation source-file options");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["sourceFile"], "customer_accounts.csv");
    assert!(
        files[0]["columns"]
            .as_array()
            .is_some_and(|columns| { columns.iter().all(|column| column["selected"] == false) })
    );
    let key_column = files[0]["columns"]
        .as_array()
        .expect("remediation key columns")
        .iter()
        .find(|column| column["sourceColumn"] == "migrationPartnerId")
        .expect("mapped canonical key suggestion");
    assert_eq!(key_column["sourceColumnOrdinal"], 0);
    assert_eq!(key_column["suggested"], true);

    let selection = json!({
        "entityId": files[0]["entityId"],
        "sourceFile": files[0]["sourceFile"],
        "sourceColumnOrdinal": key_column["sourceColumnOrdinal"],
        "sourceColumn": key_column["sourceColumn"]
    });
    let stale = command_json(
        app,
        Method::POST,
        "/api/v1/engagement/remediation-packs",
        "engagement-remediation-pack-create-v1",
        "fresh-remediation-stale-options-0001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/create-remediation-pack/v1",
            "expectedEngagementGeneration": options.body["engagementGeneration"]
                .as_u64()
                .expect("remediation options generation") + 1,
            "expectedValidationRunId": options.body["validationRunId"],
            "expectedOptionsHash": options.body["optionsHash"],
            "selectedKeyColumns": [selection.clone()],
            "actor": "Synthetic remediation test practitioner",
            "reason": "A stale options command must fail before creating remediation authority.",
            "localDataExportAcknowledgement": "migration-stream/local-remediation-data-export/v1",
            "includeOffendingValues": false,
            "idempotencyKey": "fresh-remediation-stale-options-0001"
        }),
    )
    .await;
    assert_eq!(stale.status, StatusCode::CONFLICT, "{:?}", stale.body);
    let empty_history = get_json(app, "/api/v1/engagement/remediation-packs").await;
    assert_eq!(empty_history.status, StatusCode::OK);
    assert_eq!(empty_history.body["totalCount"], 0);
    assert!(
        empty_history.body["items"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );

    let keys_only_command = json!({
        "schemaVersion": "migration-stream/engagement/write/create-remediation-pack/v1",
        "expectedEngagementGeneration": options.body["engagementGeneration"],
        "expectedValidationRunId": options.body["validationRunId"],
        "expectedOptionsHash": options.body["optionsHash"],
        "selectedKeyColumns": [selection.clone()],
        "actor": "Synthetic remediation test practitioner",
        "reason": "Create an exact local remediation pack with explicitly selected business keys only.",
        "localDataExportAcknowledgement": "migration-stream/local-remediation-data-export/v1",
        "includeOffendingValues": false,
        "idempotencyKey": "fresh-remediation-keys-only-0001"
    });
    let keys_only = command_json(
        app,
        Method::POST,
        "/api/v1/engagement/remediation-packs",
        "engagement-remediation-pack-create-v1",
        "fresh-remediation-keys-only-0001",
        keys_only_command.clone(),
    )
    .await;
    assert_eq!(keys_only.status, StatusCode::OK, "{:?}", keys_only.body);
    assert_eq!(keys_only.body["reusedAttempt"], false);
    assert_eq!(keys_only.body["pack"]["includeOffendingValues"], false);
    assert_eq!(
        keys_only.body["pack"]["selectedKeyColumns"],
        json!([selection.clone()])
    );
    let immediate_keys_only_id = keys_only.body["pack"]["packId"]
        .as_str()
        .expect("immediate keys-only remediation pack ID");
    let database = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open remediation diagnostic database");
    let (archive_relative_path, archive_sha256, archive_size): (String, String, u64) = database
        .query_row(
            "SELECT archive_relative_path, archive_sha256, archive_size
             FROM engagement_remediation_packs WHERE pack_id = ?1",
            [immediate_keys_only_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("load exact remediation archive authority");
    let archive_path = fixture.workspace.join(archive_relative_path);
    let archive_metadata =
        fs::symlink_metadata(&archive_path).expect("load exact remediation archive metadata");
    assert!(archive_metadata.file_type().is_file());
    assert_eq!(archive_metadata.len(), archive_size);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        assert_eq!(archive_metadata.nlink(), 1);
        assert_eq!(archive_metadata.permissions().mode() & 0o777, 0o400);
    }
    assert_eq!(
        migration_stream::canonical::lower_hex(Sha256::digest(
            fs::read(&archive_path).expect("read exact remediation archive")
        )),
        archive_sha256
    );
    let (immediate_status, immediate_bytes) = get_bytes(
        app,
        &format!("/api/v1/engagement/remediation-packs/{immediate_keys_only_id}/download"),
    )
    .await;
    assert_eq!(
        immediate_status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&immediate_bytes)
    );

    let keys_only_retry = command_json(
        app,
        Method::POST,
        "/api/v1/engagement/remediation-packs",
        "engagement-remediation-pack-create-v1",
        "fresh-remediation-keys-only-0001",
        keys_only_command.clone(),
    )
    .await;
    assert_eq!(
        keys_only_retry.status,
        StatusCode::OK,
        "{:?}",
        keys_only_retry.body
    );
    assert_eq!(keys_only_retry.body["reusedAttempt"], true);
    assert_eq!(
        keys_only_retry.body["pack"]["packId"],
        keys_only.body["pack"]["packId"]
    );
    assert_eq!(
        keys_only_retry.body["pack"]["archiveSha256"],
        keys_only.body["pack"]["archiveSha256"]
    );

    let offending_command = json!({
        "schemaVersion": "migration-stream/engagement/write/create-remediation-pack/v1",
        "expectedEngagementGeneration": options.body["engagementGeneration"],
        "expectedValidationRunId": options.body["validationRunId"],
        "expectedOptionsHash": options.body["optionsHash"],
        "selectedKeyColumns": [selection],
        "actor": "Synthetic remediation test practitioner",
        "reason": "Create a distinct exact local remediation pack with separately authorized offending values.",
        "localDataExportAcknowledgement": "migration-stream/local-remediation-data-export/v1",
        "includeOffendingValues": true,
        "offendingValuesAcknowledgement": "migration-stream/local-remediation-offending-values-export/v1",
        "idempotencyKey": "fresh-remediation-offending-values-0001"
    });
    let concurrent_start = Arc::new(Barrier::new(3));
    let first_app = app.clone();
    let first_start = Arc::clone(&concurrent_start);
    let first_command = offending_command.clone();
    let retry_command = offending_command.clone();
    let first = tokio::spawn(async move {
        first_start.wait().await;
        command_json(
            &first_app,
            Method::POST,
            "/api/v1/engagement/remediation-packs",
            "engagement-remediation-pack-create-v1",
            "fresh-remediation-offending-values-0001",
            first_command,
        )
        .await
    });
    let second_app = app.clone();
    let second_start = Arc::clone(&concurrent_start);
    let second = tokio::spawn(async move {
        second_start.wait().await;
        command_json(
            &second_app,
            Method::POST,
            "/api/v1/engagement/remediation-packs",
            "engagement-remediation-pack-create-v1",
            "fresh-remediation-offending-values-0001",
            offending_command,
        )
        .await
    });
    concurrent_start.wait().await;
    let (first_response, second_response) = tokio::join!(first, second);
    let first_response = first_response.expect("first concurrent remediation response");
    let second_response = second_response.expect("second concurrent remediation response");
    let (offending, concurrent_replay) = match (first_response.status, second_response.status) {
        (StatusCode::OK, StatusCode::OK) => {
            if first_response.body["reusedAttempt"] == false {
                (first_response, Some(second_response))
            } else {
                (second_response, Some(first_response))
            }
        }
        (StatusCode::OK, StatusCode::CONFLICT) => {
            assert_eq!(second_response.body["error"]["code"], "WORKSPACE_LOCKED");
            (first_response, None)
        }
        (StatusCode::CONFLICT, StatusCode::OK) => {
            assert_eq!(first_response.body["error"]["code"], "WORKSPACE_LOCKED");
            (second_response, None)
        }
        statuses => panic!(
            "concurrent remediation commands did not resolve through exact replay or the bounded writer lock: {statuses:?}"
        ),
    };
    assert_eq!(offending.status, StatusCode::OK, "{:?}", offending.body);
    let concurrent_retry = if let Some(replay) = concurrent_replay {
        replay
    } else {
        command_json(
            app,
            Method::POST,
            "/api/v1/engagement/remediation-packs",
            "engagement-remediation-pack-create-v1",
            "fresh-remediation-offending-values-0001",
            retry_command,
        )
        .await
    };
    assert_eq!(concurrent_retry.status, StatusCode::OK);
    assert_eq!(concurrent_retry.body["reusedAttempt"], true);
    assert_eq!(
        concurrent_retry.body["pack"]["packId"],
        offending.body["pack"]["packId"]
    );
    assert_eq!(
        concurrent_retry.body["pack"]["archiveSha256"],
        offending.body["pack"]["archiveSha256"]
    );
    assert_eq!(offending.body["reusedAttempt"], false);
    assert_eq!(offending.body["pack"]["includeOffendingValues"], true);
    assert_ne!(
        offending.body["pack"]["packId"],
        keys_only.body["pack"]["packId"]
    );

    let received_values = [
        "CUST-002",
        "CUST-003",
        "Valid Organization",
        "Missing identifier and organization name",
        "Over length",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ];
    for broad in [&options.body, &keys_only.body, &offending.body] {
        let serialized = broad.to_string();
        for received in received_values {
            assert!(
                !serialized.contains(received),
                "broad remediation response leaked received value {received}"
            );
        }
    }

    let history = get_json(app, "/api/v1/engagement/remediation-packs").await;
    assert_eq!(history.status, StatusCode::OK, "{:?}", history.body);
    assert_eq!(
        history.body["schemaVersion"],
        "migration-stream/engagement/remediation-pack-history/v1"
    );
    assert_eq!(history.body["totalCount"], 2);
    assert_eq!(history.body["returnedCount"], 2);
    assert_eq!(history.body["truncated"], false);
    assert_eq!(history.body["items"].as_array().map(Vec::len), Some(2));
    let history_text = history.body.to_string();
    for received in received_values {
        assert!(
            !history_text.contains(received),
            "remediation history leaked received value {received}"
        );
    }
    assert!(history_text.contains("migrationPartnerId"));

    let keys_only_id = keys_only.body["pack"]["packId"]
        .as_str()
        .expect("keys-only remediation pack ID");
    let (keys_only_status, keys_only_zip) = get_bytes(
        app,
        &format!("/api/v1/engagement/remediation-packs/{keys_only_id}/download"),
    )
    .await;
    assert_eq!(
        keys_only_status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&keys_only_zip)
    );
    assert_eq!(
        migration_stream::canonical::lower_hex(Sha256::digest(&keys_only_zip)),
        keys_only.body["pack"]["archiveSha256"]
    );
    let keys_only_issues: Value = serde_json::from_slice(&remediation_zip_member(
        &keys_only_zip,
        "remediation-issues.json",
    ))
    .expect("parse keys-only remediation issues");
    assert_eq!(keys_only_issues["includeOffendingValues"], false);
    assert!(keys_only_issues["issues"].as_array().is_some_and(|issues| {
        !issues.is_empty()
            && issues.iter().all(|issue| {
                issue["selectedKeys"].as_array().is_some_and(|keys| {
                    keys.len() == 1 && keys[0]["sourceColumn"] == "migrationPartnerId"
                }) && issue["offendingValue"].get("value").is_none()
            })
    }));
    let keys_only_payload = String::from_utf8(remediation_zip_member(
        &keys_only_zip,
        "remediation-issues.json",
    ))
    .expect("keys-only remediation payload UTF-8");
    for unselected in [
        "Valid Organization",
        "Missing identifier and organization name",
        "Over length",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        assert!(
            !keys_only_payload.contains(unselected),
            "keys-only archive leaked unselected value {unselected}"
        );
    }

    let offending_id = offending.body["pack"]["packId"]
        .as_str()
        .expect("offending-values remediation pack ID");
    let (offending_status, offending_zip) = get_bytes(
        app,
        &format!("/api/v1/engagement/remediation-packs/{offending_id}/download"),
    )
    .await;
    assert_eq!(
        offending_status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&offending_zip)
    );
    assert_eq!(
        migration_stream::canonical::lower_hex(Sha256::digest(&offending_zip)),
        offending.body["pack"]["archiveSha256"]
    );
    let offending_issues: Value = serde_json::from_slice(&remediation_zip_member(
        &offending_zip,
        "remediation-issues.json",
    ))
    .expect("parse offending-values remediation issues");
    assert_eq!(offending_issues["includeOffendingValues"], true);
    assert!(offending_issues["issues"].as_array().is_some_and(|issues| {
        issues.iter().any(|issue| {
            issue["offendingValue"]["applicability"] == "EXACT_SOURCE_COLUMN"
                && issue["offendingValue"]["sourceColumn"] == "organizationName1"
                && issue["offendingValue"]["value"]
                    == "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        })
    }));
    let offending_payload = String::from_utf8(remediation_zip_member(
        &offending_zip,
        "remediation-issues.json",
    ))
    .expect("offending-values remediation payload UTF-8");
    for unselected in [
        "Valid Organization",
        "Missing identifier and organization name",
        "Over length",
    ] {
        assert!(
            !offending_payload.contains(unselected),
            "offending-values archive leaked unrelated value {unselected}"
        );
    }

    let reopened = application(fixture);
    let reopened_history = get_json(&reopened, "/api/v1/engagement/remediation-packs").await;
    assert_eq!(reopened_history.status, StatusCode::OK);
    assert_eq!(reopened_history.body, history.body);
    let (reopened_status, reopened_zip) = get_bytes(
        &reopened,
        &format!("/api/v1/engagement/remediation-packs/{offending_id}/download"),
    )
    .await;
    assert_eq!(reopened_status, StatusCode::OK);
    assert_eq!(reopened_zip, offending_zip);

    let database = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("open remediation tamper proof database");
    let (original_selected_columns, original_record): (String, String) = database
        .query_row(
            "SELECT selected_columns_json, canonical_record_json
             FROM engagement_remediation_packs WHERE pack_id = ?1",
            [keys_only_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read original remediation pack authority");
    let replace_pack_column = |column: &str, value: &str| {
        assert!(matches!(
            column,
            "selected_columns_json" | "canonical_record_json"
        ));
        database
            .execute_batch("DROP TRIGGER engagement_remediation_packs_no_update;")
            .expect("open bounded remediation pack tamper window");
        database
            .execute(
                &format!(
                    "UPDATE engagement_remediation_packs SET {column} = ?1 WHERE pack_id = ?2"
                ),
                rusqlite::params![value, keys_only_id],
            )
            .expect("apply bounded remediation pack tamper");
        database
            .execute_batch(
                "CREATE TRIGGER engagement_remediation_packs_no_update
                 BEFORE UPDATE ON engagement_remediation_packs
                 BEGIN SELECT RAISE(ABORT, 'remediation packs are immutable'); END;",
            )
            .expect("restore remediation pack immutability guard");
    };
    let mut selected_tamper: Value = serde_json::from_str(&original_selected_columns)
        .expect("parse original remediation key selections");
    selected_tamper[0]["sourceColumn"] = json!("CUST-002");
    let selected_tamper = migration_stream::canonical::canonical_json_string(&selected_tamper)
        .expect("canonical selected-column tamper");
    replace_pack_column("selected_columns_json", &selected_tamper);
    let rejected_history = get_json(&reopened, "/api/v1/engagement/remediation-packs").await;
    assert_eq!(rejected_history.status, StatusCode::CONFLICT);
    replace_pack_column("selected_columns_json", &original_selected_columns);

    let mut record_tamper: Value =
        serde_json::from_str(&original_record).expect("parse original remediation record");
    record_tamper["actor"] = json!("CUST-002");
    let record_tamper = migration_stream::canonical::canonical_json_string(&record_tamper)
        .expect("canonical remediation record tamper");
    replace_pack_column("canonical_record_json", &record_tamper);
    let rejected_retry = command_json(
        &reopened,
        Method::POST,
        "/api/v1/engagement/remediation-packs",
        "engagement-remediation-pack-create-v1",
        "fresh-remediation-keys-only-0001",
        keys_only_command.clone(),
    )
    .await;
    assert_eq!(rejected_retry.status, StatusCode::CONFLICT);
    let rejected_record_history = get_json(&reopened, "/api/v1/engagement/remediation-packs").await;
    assert_eq!(rejected_record_history.status, StatusCode::CONFLICT);
    replace_pack_column("canonical_record_json", &original_record);
    let restored_history = get_json(&reopened, "/api/v1/engagement/remediation-packs").await;
    assert_eq!(restored_history.status, StatusCode::OK);
    assert_eq!(restored_history.body, history.body);
    BaselineRemediationProof {
        history: history.body,
        archives: vec![
            (keys_only_id.to_owned(), keys_only_zip),
            (offending_id.to_owned(), offending_zip),
        ],
        keys_only_command,
        keys_only_pack: keys_only.body["pack"].clone(),
    }
}

async fn runtime_identity_request(app: &Router, token: Option<&str>) -> (StatusCode, Vec<u8>) {
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri("/__migration-stream/runtime-identity")
        .header("host", HOST);
    if let Some(token) = token {
        builder = builder.header("x-migration-stream-runtime-token", token);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).expect("build identity request"))
        .await
        .expect("infallible assisted runtime router");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read bounded identity response")
        .to_vec();
    (status, bytes)
}

#[tokio::test]
async fn assisted_runtime_identity_is_ready_before_workspace_initialization() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let fixture = fixture("assisted-runtime-empty-workspace");
    let token = if cfg!(windows) {
        "0123456789abcdef".repeat(4)
    } else {
        "0123456789abcdef".repeat(2)
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind exact raw identity listener");
    let address = listener.local_addr().expect("read identity listen address");
    let app = assisted_runtime_application_at(&fixture, &token, &address.to_string());

    let (status, bytes) = runtime_identity_request(&app, Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let identity: Value = serde_json::from_slice(&bytes).expect("parse startup runtime identity");
    assert_eq!(
        bytes,
        migration_stream::canonical::canonical_json_bytes(&identity)
            .expect("serialize canonical startup runtime identity")
    );
    assert_eq!(
        identity["schemaVersion"],
        "migration-stream/assisted-local-runtime/application-identity/v2"
    );
    assert!(
        identity["applicationIdentitySha256"]
            .as_str()
            .is_some_and(|value| value.len() == 64)
    );

    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve exact raw identity endpoint");
    });
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect exact raw identity endpoint");
    let request = format!(
        "GET /__migration-stream/runtime-identity HTTP/1.1\r\nHost: {address}\r\nX-Migration-Stream-Runtime-Token: {token}\r\nConnection: close\r\nAccept: application/json\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write raw identity request");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read raw identity response");
    server.abort();
    let _ = server.await;
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .expect("raw identity response headers");
    let headers = std::str::from_utf8(&response[..header_end]).expect("identity response headers");
    assert!(headers.starts_with("HTTP/1.1 200 "), "{headers}");
    let raw_identity: Value =
        serde_json::from_slice(&response[header_end..]).expect("parse raw identity response body");
    assert_eq!(
        response[header_end..],
        migration_stream::canonical::canonical_json_bytes(&raw_identity)
            .expect("serialize canonical raw identity response")
    );
}

#[tokio::test]
async fn assisted_runtime_identity_is_token_bound_value_free_and_tracks_authority() {
    let fixture = fixture("assisted-runtime-identity");
    migration_stream::object_workspace::establish_for_local_automation(
        &fixture.workspace,
        "Assisted runtime identity workspace",
        "Synthetic customer readiness",
        "Runtime identity test operator",
    )
    .expect("establish object workspace");
    let token = if cfg!(windows) {
        "0123456789abcdef".repeat(4)
    } else {
        "0123456789abcdef".repeat(2)
    };
    let wrong_token = if cfg!(windows) {
        "fedcba9876543210".repeat(4)
    } else {
        "fedcba9876543210".repeat(2)
    };
    let app = assisted_runtime_application(&fixture, &token);

    for presented in [None, Some(wrong_token.as_str())] {
        let (status, body) = runtime_identity_request(&app, presented).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body.is_empty(),
            "failed authentication must disclose no body"
        );
    }

    let (status, before_bytes) = runtime_identity_request(&app, Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let before: Value = serde_json::from_slice(&before_bytes).expect("parse runtime identity");
    assert_eq!(
        before_bytes.as_slice(),
        migration_stream::canonical::canonical_json_bytes(&before)
            .expect("serialize canonical runtime identity"),
        "the authenticated runtime identity wire body must be canonical JSON"
    );
    assert_eq!(
        before["schemaVersion"],
        "migration-stream/assisted-local-runtime/application-identity/v2"
    );
    assert_eq!(before["listenAddress"], HOST);
    #[cfg(windows)]
    {
        let expected_workspace =
            migration_stream_windows_native::RealDirectory::capture(&fixture.workspace)
                .expect("capture Windows workspace identity");
        assert_eq!(
            Path::new(
                before["workspacePath"]
                    .as_str()
                    .expect("runtime workspace path")
            ),
            expected_workspace.path(),
            "the runtime identity must use the retained native workspace spelling"
        );
        assert_eq!(
            before["filesystemIdentitySha256"],
            migration_stream_windows_native::windows_workspace_filesystem_identity_sha256(
                expected_workspace.identity()
            ),
            "the runtime identity path and digest must come from the same native capture"
        );
    }
    assert_eq!(
        before["sessionTokenSha256"],
        migration_stream::canonical::lower_hex(Sha256::digest(token.as_bytes()))
    );
    assert!(
        before["applicationIdentitySha256"]
            .as_str()
            .is_some_and(|value| value.len() == 64)
    );
    let serialized = String::from_utf8(before_bytes).expect("identity response UTF-8");
    assert!(!serialized.contains(&token));
    assert!(!serialized.contains("journeyBinding"));
    assert!(!serialized.contains("received"));

    let _ = create_and_reserve(
        &app,
        "Runtime identity authority transition",
        "synthetic.zip",
        "runtime-identity-create-0001",
        "runtime-identity-reserve-0001",
    )
    .await;
    let (status, after_bytes) = runtime_identity_request(&app, Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let after: Value =
        serde_json::from_slice(&after_bytes).expect("parse changed runtime identity");
    assert_ne!(
        before["applicationIdentitySha256"], after["applicationIdentitySha256"],
        "engagement authority transition must change the value-free runtime identity"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn empty_startup_folder_requires_explicit_workspace_initialization_and_reopens() {
    let fixture = fixture("object-workspace-initialize");
    let entries_before = fs::read_dir(&fixture.workspace)
        .expect("read empty selected workspace")
        .map(|entry| entry.expect("workspace entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(entries_before, [] as [std::ffi::OsString; 0]);
    let app = application_without_establishing_workspace(&fixture);

    let initial = get_json(&app, "/api/v1/object-workspace").await;
    assert_eq!(initial.status, StatusCode::OK);
    assert_eq!(initial.body["state"], "INITIALIZE_REQUIRED");
    assert_eq!(initial.body["requiredAction"], "INITIALIZE");
    assert!(initial.body.get("workspace").is_some_and(Value::is_null));
    assert!(
        !initial
            .body
            .to_string()
            .contains(fixture.workspace.to_string_lossy().as_ref())
    );

    let locked = get_json(&app, "/api/v1/journey").await;
    assert_eq!(locked.status, StatusCode::OK);
    assert_eq!(locked.body["schemaVersion"], "migration-stream/journey/v2");
    assert_eq!(locked.body["currentStageId"], "SETUP");
    assert_eq!(locked.body["workspace"]["state"], "INITIALIZE_REQUIRED");
    assert!(
        locked.body["stages"].as_array().expect("journey stages")[1..]
            .iter()
            .all(|stage| stage["status"] == "LOCKED"
                && stage["href"].is_null()
                && stage["unavailableReason"]
                    == "Initialize this workspace before accepting source evidence.")
    );
    let blocked_create = command_json(
        &app,
        Method::POST,
        "/api/v1/engagement",
        "engagement-create-v1",
        "blocked-before-workspace-0001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/create/v1",
            "displayName": "Must not be created",
            "idempotencyKey": "blocked-before-workspace-0001"
        }),
    )
    .await;
    assert_eq!(blocked_create.status, StatusCode::CONFLICT);
    let blocked_packs = get_json(&app, "/api/v1/engagement/object-packs").await;
    assert_eq!(blocked_packs.status, StatusCode::CONFLICT);
    let entries_after_inactive_reads = fs::read_dir(&fixture.workspace)
        .expect("read inactive selected workspace")
        .map(|entry| entry.expect("workspace entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(
        entries_after_inactive_reads, entries_before,
        "router construction and inactive lifecycle reads must not create a database, lock, descriptor, or runtime namespace"
    );

    let initialize = command_json(
        &app,
        Method::POST,
        "/api/v1/object-workspace/initialize",
        "object-workspace-initialize-v1",
        "workspace-initialize-0001",
        json!({
            "schemaVersion": "migration-stream/object-workspace/write/initialize/v1",
            "expectedStateHash": initial.body["stateHash"],
            "displayName": "Customer readiness workspace",
            "migrationObjectNameOrScope": "Customer master readiness",
            "creatorAttribution": "Synthetic browser operator",
            "idempotencyKey": "workspace-initialize-0001"
        }),
    )
    .await;
    assert_eq!(initialize.status, StatusCode::OK, "{:?}", initialize.body);
    assert_eq!(
        initialize.body["displayName"],
        "Customer readiness workspace"
    );
    assert_eq!(initialize.body["generation"], 1);
    let workspace_id = initialize.body["workspaceId"]
        .as_str()
        .expect("workspace ID")
        .to_owned();
    let descriptor_hash = initialize.body["descriptorHash"]
        .as_str()
        .expect("descriptor hash")
        .to_owned();

    let create = command_json(
        &app,
        Method::POST,
        "/api/v1/engagement",
        "engagement-create-v1",
        "engagement-create-after-workspace-0001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/create/v1",
            "displayName": "Customer readiness engagement",
            "idempotencyKey": "engagement-create-after-workspace-0001"
        }),
    )
    .await;
    assert_eq!(create.status, StatusCode::OK, "{:?}", create.body);
    let engagement_id = create.body["engagementId"]
        .as_str()
        .expect("engagement ID")
        .to_owned();

    let reopened = application_without_establishing_workspace(&fixture);
    let status = get_json(&reopened, "/api/v1/object-workspace").await;
    assert_eq!(status.body["state"], "ACTIVE");
    assert_eq!(status.body["workspace"]["workspaceId"], workspace_id);
    assert_eq!(status.body["workspace"]["descriptorHash"], descriptor_hash);
    let context = get_json(&reopened, "/api/v1/context").await;
    assert_eq!(context.body["engagement"]["engagementId"], engagement_id);
    let journey = get_json(&reopened, "/api/v1/journey").await;
    assert_eq!(
        journey.body["workspace"]["workspace"]["workspaceId"],
        workspace_id
    );
    assert_eq!(
        journey.body["workspace"]["workspace"]["descriptorHash"],
        descriptor_hash
    );
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open bound database");
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TRIGGER object_workspace_engagement_bindings_no_update;
             UPDATE object_workspace_engagement_bindings
             SET engagement_id = 'tampered-engagement'
             WHERE singleton = 1;",
        )
        .expect("simulate engagement-link tamper");
    drop(connection);
    let rejected = get_json(&reopened, "/api/v1/object-workspace").await;
    assert_eq!(rejected.status, StatusCode::CONFLICT);
    let integrity =
        migration_stream::state::integrity_check(&fixture.workspace).expect("tamper replay");
    assert!(!integrity.ok);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn existing_pre_descriptor_evidence_requires_adoption_and_is_preserved() {
    let fixture = fixture("object-workspace-adopt");
    migration_stream::object_workspace::establish_for_local_automation(
        &fixture.workspace,
        "Pre-descriptor synthetic fixture",
        "Customer legacy evidence",
        "Legacy fixture builder",
    )
    .expect("create current schema through the real authority");
    let database_path = fixture.workspace.join("state.sqlite3");
    let connection = Connection::open(&database_path).expect("open legacy database");
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TRIGGER object_workspace_engagement_bindings_no_update;
             DROP TRIGGER object_workspace_engagement_bindings_no_delete;
             DROP TABLE object_workspace_engagement_bindings;
             DROP TRIGGER object_workspace_bindings_no_update;
             DROP TRIGGER object_workspace_bindings_no_delete;
             DROP TABLE object_workspace_bindings;",
        )
        .expect("remove only Product Spine workspace authority from copied legacy fixture");
    let descriptor = fixture.workspace.join("workspace.json");
    make_owner_writable(&descriptor);
    fs::remove_file(descriptor).expect("remove descriptor from copied legacy fixture");
    connection
        .execute(
            "INSERT INTO jobs (
                 id, state, source_name, source_path, policy_json,
                 created_at, updated_at
             ) VALUES (
                 'legacy-job', 'SPOOLING', 'synthetic.zip',
                 'legacy/source.zip', '{}',
                 '2026-07-29T00:00:00.000Z',
                 '2026-07-29T00:00:00.000Z'
             )",
            [],
        )
        .expect("insert legacy evidence");
    let legacy_evidence: (String, String, String) = connection
        .query_row(
            "SELECT id, source_name, policy_json FROM jobs WHERE id = 'legacy-job'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("snapshot legacy evidence");
    drop(connection);
    let legacy_file = fixture.workspace.join("legacy-evidence.bin");
    fs::write(&legacy_file, b"immutable synthetic legacy evidence")
        .expect("write legacy evidence file");
    let legacy_file_hash = migration_stream::canonical::lower_hex(Sha256::digest(
        fs::read(&legacy_file).expect("read legacy evidence file"),
    ));

    let app = application_without_establishing_workspace(&fixture);
    let initial = get_json(&app, "/api/v1/object-workspace").await;
    assert_eq!(initial.body["state"], "ADOPT_REQUIRED");
    assert_eq!(initial.body["requiredAction"], "ADOPT");
    let wrong_mode = command_json(
        &app,
        Method::POST,
        "/api/v1/object-workspace/initialize",
        "object-workspace-initialize-v1",
        "workspace-wrong-mode-0001",
        json!({
            "schemaVersion": "migration-stream/object-workspace/write/initialize/v1",
            "expectedStateHash": initial.body["stateHash"],
            "displayName": "Adopted readiness workspace",
            "migrationObjectNameOrScope": "Customer legacy evidence",
            "creatorAttribution": "Synthetic adopting operator",
            "idempotencyKey": "workspace-wrong-mode-0001"
        }),
    )
    .await;
    assert_eq!(wrong_mode.status, StatusCode::CONFLICT);
    assert!(!fixture.workspace.join("workspace.json").exists());

    let adopted = command_json(
        &app,
        Method::POST,
        "/api/v1/object-workspace/adopt",
        "object-workspace-adopt-v1",
        "workspace-adopt-0001",
        json!({
            "schemaVersion": "migration-stream/object-workspace/write/adopt/v1",
            "expectedStateHash": initial.body["stateHash"],
            "displayName": "Adopted readiness workspace",
            "migrationObjectNameOrScope": "Customer legacy evidence",
            "creatorAttribution": "Synthetic adopting operator",
            "idempotencyKey": "workspace-adopt-0001"
        }),
    )
    .await;
    assert_eq!(adopted.status, StatusCode::OK, "{:?}", adopted.body);
    assert_eq!(adopted.body["establishedBy"], "ADOPTED");
    let connection = Connection::open(&database_path).expect("reopen adopted database");
    let preserved: (String, String, String) = connection
        .query_row(
            "SELECT id, source_name, policy_json FROM jobs WHERE id = 'legacy-job'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read preserved legacy evidence");
    assert_eq!(preserved, legacy_evidence);
    drop(connection);
    assert_eq!(
        migration_stream::canonical::lower_hex(Sha256::digest(
            fs::read(&legacy_file).expect("reread legacy evidence file")
        )),
        legacy_file_hash,
        "adoption must preserve pre-existing evidence bytes"
    );
    let integrity =
        migration_stream::state::integrity_check(&fixture.workspace).expect("global integrity");
    assert!(integrity.ok, "{:?}", integrity.issues);
    assert_eq!(integrity.checked_object_workspaces, 1);
}

async fn create_and_preflight(
    app: &Router,
    display_name: &str,
    source_name: &str,
    bytes: Vec<u8>,
) -> (String, Value) {
    let created = command_json(
        app,
        Method::POST,
        "/api/v1/engagement",
        "engagement-create-v1",
        "test-create-00000001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/create/v1",
            "displayName": display_name,
            "idempotencyKey": "test-create-00000001"
        }),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);
    assert_eq!(
        created.body["schemaVersion"],
        "migration-stream/engagement/record/v1"
    );

    let reserved = command_json(
        app,
        Method::POST,
        "/api/v1/engagement/source-preflights",
        "engagement-source-preflight-reserve-v1",
        "test-reserve-0000001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/reserve-source-preflight/v1",
            "sourceName": source_name,
            "idempotencyKey": "test-reserve-0000001"
        }),
    )
    .await;
    assert_eq!(reserved.status, StatusCode::OK, "{:?}", reserved.body);
    let preflight_id = reserved.body["preflight"]["preflightId"]
        .as_str()
        .expect("preflight ID")
        .to_owned();
    let mismatched_upload =
        upload_zip(app, &preflight_id, "test-wrong-upload-0001", bytes.clone()).await;
    assert_eq!(mismatched_upload.status, StatusCode::CONFLICT);
    assert_eq!(mismatched_upload.body["error"]["code"], "CONFLICT");
    let uploaded = upload_zip(app, &preflight_id, "test-reserve-0000001", bytes).await;
    (preflight_id, uploaded.body)
}

async fn create_and_reserve(
    app: &Router,
    display_name: &str,
    source_name: &str,
    create_key: &str,
    reserve_key: &str,
) -> (String, Value) {
    let created = command_json(
        app,
        Method::POST,
        "/api/v1/engagement",
        "engagement-create-v1",
        create_key,
        json!({
            "schemaVersion": "migration-stream/engagement/write/create/v1",
            "displayName": display_name,
            "idempotencyKey": create_key
        }),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);
    let reserved = command_json(
        app,
        Method::POST,
        "/api/v1/engagement/source-preflights",
        "engagement-source-preflight-reserve-v1",
        reserve_key,
        json!({
            "schemaVersion": "migration-stream/engagement/write/reserve-source-preflight/v1",
            "sourceName": source_name,
            "idempotencyKey": reserve_key
        }),
    )
    .await;
    assert_eq!(reserved.status, StatusCode::OK, "{:?}", reserved.body);
    (
        reserved.body["preflight"]["preflightId"]
            .as_str()
            .expect("preflight ID")
            .to_owned(),
        reserved.body,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::too_many_lines)]
async fn fresh_sanitized_package_reaches_existing_issues_and_reopens() {
    let fixture = fixture("fresh-golden");
    let app = application(&fixture);
    fs::write(
        fixture
            .object_packs
            .join("synthetic.customer.readiness/1.0.0/synthetic-profile.reviewed.json"),
        "{}",
    )
    .expect("mutate the post-start catalogue path");
    let initial_context = get_json(&app, "/api/v1/context").await;
    assert_eq!(initial_context.status, StatusCode::OK);
    assert_eq!(
        initial_context.body["schemaVersion"],
        "migration-stream/engagement/context/v1"
    );
    assert_eq!(initial_context.body["capabilities"]["targetWrite"], false);
    assert_eq!(initial_context.body["capabilities"]["publication"], false);
    let initial_journey = get_json(&app, "/api/v1/journey").await;
    assert_eq!(initial_journey.status, StatusCode::OK);
    assert_eq!(
        initial_journey.body["schemaVersion"],
        "migration-stream/journey/v2"
    );
    assert_eq!(initial_journey.body["currentStageId"], "SETUP");
    assert_eq!(initial_journey.body["binding"]["engagementId"], Value::Null);
    assert_eq!(
        initial_journey.body["stages"].as_array().map(Vec::len),
        Some(10)
    );

    let csv = concat!(
        "migrationPartnerId,category,organizationName1,notes\n",
        ",2,,Missing identifier and organization name\n",
        "CUST-002,2,Valid Organization,\n",
        "CUST-003,1,",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ",Over length\n"
    );
    let package = source_package(csv);
    let (preflight_id, preflight) = create_and_preflight(
        &app,
        "Unfamiliar sanitized readiness proof",
        "unfamiliar.zip",
        package,
    )
    .await;
    assert_eq!(preflight["state"], "READY", "{preflight:?}");
    assert_eq!(
        preflight["manifest"]["csv_paths"],
        json!(["customer_accounts.csv"])
    );
    let source_hash = preflight["sourceHash"]
        .as_str()
        .expect("preflight source hash");
    let manifest_hash = preflight["manifestHash"]
        .as_str()
        .expect("preflight manifest hash");
    assert_eq!(source_hash.len(), 64);
    assert_eq!(manifest_hash.len(), 64);

    let accepted = command_json(
        &app,
        Method::POST,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/accept"),
        "engagement-source-accept-v1",
        "test-accept-0000001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/accept-source/v1",
            "preflightId": preflight_id,
            "expectedSourceHash": source_hash,
            "expectedManifestHash": manifest_hash,
            "idempotencyKey": "test-accept-0000001",
            "acknowledgement": "migration-stream/source-acceptance/v1"
        }),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{:?}", accepted.body);
    let job_id = accepted.body["jobId"]
        .as_str()
        .expect("accepted job ID")
        .to_owned();
    assert_eq!(accepted.body["sourceHash"], source_hash);
    let profiled_journey = get_json(&app, "/api/v1/journey").await;
    assert_eq!(profiled_journey.status, StatusCode::OK);
    assert_eq!(profiled_journey.body["currentStageId"], "DESTINATION");
    assert_eq!(profiled_journey.body["binding"]["jobId"], job_id);
    assert_eq!(
        profiled_journey.body["binding"]["profilingRunId"],
        accepted.body["profilingRunId"]
    );
    assert_eq!(profiled_journey.body["binding"]["sourceHash"], source_hash);
    assert_eq!(
        profiled_journey.body["binding"]["manifestHash"],
        manifest_hash
    );
    assert_eq!(profiled_journey.body["stages"][2]["status"], "COMPLETED");

    let before_closed_intake = get_json(&app, "/api/v1/engagement").await;
    let replacement_reservation = command_json(
        &app,
        Method::POST,
        "/api/v1/engagement/source-preflights",
        "engagement-source-preflight-reserve-v1",
        "closed-intake-reserve1",
        json!({
            "schemaVersion": "migration-stream/engagement/write/reserve-source-preflight/v1",
            "sourceName": "replacement.zip",
            "idempotencyKey": "closed-intake-reserve1"
        }),
    )
    .await;
    assert_eq!(replacement_reservation.status, StatusCode::CONFLICT);
    assert_eq!(replacement_reservation.body["error"]["code"], "CONFLICT");
    let after_closed_intake = get_json(&app, "/api/v1/engagement").await;
    assert_eq!(
        after_closed_intake.body["state"]["generation"],
        before_closed_intake.body["state"]["generation"]
    );
    assert_eq!(
        after_closed_intake.body["state"]["active"]["jobId"],
        before_closed_intake.body["state"]["active"]["jobId"]
    );
    let closed_intake_context = get_json(&app, "/api/v1/context").await;
    assert_eq!(
        closed_intake_context.body["capabilities"]["sourceIntake"],
        false
    );

    let profiling = get_json(&app, "/api/v1/profiling?page_size=100").await;
    assert_eq!(profiling.status, StatusCode::OK, "{:?}", profiling.body);
    assert_eq!(
        profiling.body["schema_version"],
        "migration-stream/read/profiling-summary/v1"
    );
    assert_eq!(profiling.body["scope"]["job_id"], job_id);
    assert_eq!(profiling.body["include_samples"], false);
    assert!(
        profiling.body["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    assert!(profiling.body["items"].as_array().is_some_and(|items| {
        items
            .iter()
            .all(|item| item["sample_count"] == 0 && item.get("samples").is_none())
    }));
    assert!(
        !profiling
            .body
            .to_string()
            .contains("Missing identifier and organization name")
    );
    let explicit_samples =
        get_json(&app, "/api/v1/profiling?include_samples=true&page_size=100").await;
    assert_eq!(
        explicit_samples.status,
        StatusCode::OK,
        "{:?}",
        explicit_samples.body
    );
    assert_eq!(explicit_samples.body["include_samples"], true);
    assert!(
        explicit_samples.body["items"]
            .as_array()
            .is_some_and(|items| items.iter().all(|item| {
                item["sample_count"] == 0 && item["samples"].as_array().is_some_and(Vec::is_empty)
            }))
    );

    let packs = get_json(&app, "/api/v1/engagement/object-packs").await;
    assert_eq!(packs.status, StatusCode::OK, "{:?}", packs.body);
    assert_eq!(
        packs.body["items"][0]["objectPackId"],
        "synthetic.customer.readiness"
    );
    assert!(
        packs.body["items"][0]["limitations"]
            .as_array()
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item.as_str().is_some_and(|text| {
                        text.contains("does not establish external target-system")
                    })
                })
            })
    );

    let selected = command_json(
        &app,
        Method::POST,
        "/api/v1/engagement/object-packs/select",
        "engagement-object-pack-select-v1",
        "test-select-0000001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/select-object-pack/v1",
            "objectPackId": "synthetic.customer.readiness",
            "idempotencyKey": "test-select-0000001"
        }),
    )
    .await;
    assert_eq!(selected.status, StatusCode::OK, "{:?}", selected.body);
    assert_eq!(
        selected.body["profile"]["lifecycle_status"],
        "REVIEWED_DRAFT"
    );
    assert_eq!(selected.body["profile"]["authority"], "NONE");
    assert_eq!(
        selected.body["profile"]["profile_hash"],
        "6a9954b09614e99688375f9f62c1bd8b016e424eab256f3f90e1c6e73709571f"
    );

    let approved = command_json(
        &app,
        Method::POST,
        "/api/v1/engagement/object-packs/approve",
        "engagement-profile-approval-v1",
        "test-approve-000001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/approve-object-pack-profile/v1",
            "actor": "Synthetic local test practitioner",
            "reason": "Authorize the exact reviewed synthetic profile for local mapping and validation proof only.",
            "authorityAcknowledgement": "migration-stream/local-mapping-validation-authority/v1",
            "idempotencyKey": "test-approve-000001"
        }),
    )
    .await;
    assert_eq!(approved.status, StatusCode::OK, "{:?}", approved.body);
    assert_eq!(approved.body["lifecycle_status"], "APPROVED");

    let source_rules = get_json(&app, "/api/v1/engagement/source-profile-rules").await;
    assert_eq!(
        source_rules.status,
        StatusCode::OK,
        "{:?}",
        source_rules.body
    );
    assert_eq!(source_rules.body["storedSampleValues"], false);
    assert_eq!(
        source_rules.body["sourceColumns"].as_array().map(Vec::len),
        Some(4)
    );
    assert_eq!(source_rules.body["rules"].as_array().map(Vec::len), Some(7));

    let prepared = command_json(
        &app,
        Method::POST,
        "/api/v1/engagement/mappings/prepare",
        "engagement-mapping-prepare-v1",
        "test-mapping-0000001",
        json!({
            "schemaVersion": "migration-stream/engagement/assisted-mapping-command/v1",
            "idempotencyKey": "test-mapping-0000001",
            "mappingId": "unfamiliar.sanitized.mapping",
            "version": "1.0.0",
            "selectedSuggestionIds": [
                "synthetic.customer.source-id",
                "synthetic.customer.category",
                "synthetic.customer.name",
                "synthetic.customer.notes",
                "synthetic.customer.duplicate-group"
            ],
            "author": "Synthetic local test practitioner",
            "reviewer": null,
            "acknowledgement": "migration-stream/local-assisted-mapping-reviewed-suggestions-no-target-compatibility/v1"
        }),
    )
    .await;
    assert_eq!(prepared.status, StatusCode::OK, "{:?}", prepared.body);
    assert_eq!(prepared.body["validation"]["state"], "VALID");
    let mapping_hash = prepared.body["mappingHash"]
        .as_str()
        .expect("prepared mapping hash")
        .to_owned();
    let mapped_journey = get_json(&app, "/api/v1/journey").await;
    assert_eq!(mapped_journey.status, StatusCode::OK);
    assert_eq!(mapped_journey.body["currentStageId"], "VALIDATE");
    assert_eq!(mapped_journey.body["binding"]["mappingHash"], mapping_hash);
    assert_eq!(mapped_journey.body["stages"][3]["status"], "COMPLETED");
    assert_eq!(mapped_journey.body["stages"][4]["status"], "CURRENT");

    let generic_mapping = get_json(&app, "/api/v1/mapping?page_size=200").await;
    assert_eq!(
        generic_mapping.status,
        StatusCode::NOT_FOUND,
        "{:?}",
        generic_mapping.body
    );
    let setup_mapping = get_json(&app, "/api/v1/engagement/mapping-coverage?page_size=200").await;
    assert_eq!(
        setup_mapping.status,
        StatusCode::OK,
        "{:?}",
        setup_mapping.body
    );
    assert_eq!(
        setup_mapping.body["schema_version"],
        "migration-stream/read/mapping-coverage/v1"
    );
    assert_eq!(
        setup_mapping.body["scope"],
        json!({
            "mapping_id": "unfamiliar.sanitized.mapping",
            "mapping_version": "1.0.0",
            "mapping_hash": mapping_hash
        })
    );
    assert_eq!(setup_mapping.body["mapping"]["validated_job_id"], job_id);
    assert!(
        setup_mapping.body["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );
    assert!(
        setup_mapping.body["items"]
            .as_array()
            .is_some_and(|items| items.iter().all(|item| item["issue_code"].is_null()))
    );

    fs::copy(
        project_path(
            "object-packs/synthetic.customer.readiness/1.0.0/synthetic-profile.reviewed.json",
        ),
        fixture
            .object_packs
            .join("synthetic.customer.readiness/1.0.0/synthetic-profile.reviewed.json"),
    )
    .expect("restore the trusted startup catalogue before reopening");
    let prevalidation_reopened_app = application(&fixture);
    let reopened_setup_mapping = get_json(
        &prevalidation_reopened_app,
        "/api/v1/engagement/mapping-coverage?page_size=200",
    )
    .await;
    assert_eq!(
        reopened_setup_mapping.status,
        StatusCode::OK,
        "{:?}",
        reopened_setup_mapping.body
    );
    assert_eq!(
        reopened_setup_mapping.body["canonical_hash"],
        setup_mapping.body["canonical_hash"]
    );
    let prevalidation_engagement =
        get_json(&prevalidation_reopened_app, "/api/v1/engagement").await;
    assert_eq!(
        prevalidation_engagement.status,
        StatusCode::OK,
        "{:?}",
        prevalidation_engagement.body
    );

    let launched = command_json(
        &prevalidation_reopened_app,
        Method::POST,
        "/api/v1/engagement/validation/launch",
        "engagement-validation-launch-v1",
        "test-validation-0001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/launch-validation/v1",
            "idempotencyKey": "test-validation-0001",
            "expectedEngagementGeneration": prevalidation_engagement.body["state"]["generation"],
            "expectedJobId": job_id,
            "expectedMappingId": "unfamiliar.sanitized.mapping",
            "expectedMappingVersion": "1.0.0",
            "expectedMappingHash": mapping_hash
        }),
    )
    .await;
    assert_eq!(launched.status, StatusCode::OK, "{:?}", launched.body);
    assert_eq!(launched.body["status"]["state"], "COMPLETE");
    assert!(
        launched.body["status"]["issueCount"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );

    let validation_run_id = launched.body["status"]["runId"]
        .as_str()
        .expect("completed validation run ID")
        .to_owned();
    let reconciliation = ReconciliationApprovalService::new(&fixture.workspace);
    let snapshot = reconciliation
        .calculate(&CalculateReconciliationRequest {
            schema_version: "migration-stream/write/calculate-reconciliation/v1".to_owned(),
            job_id: job_id.clone(),
            validation_run_id: validation_run_id.clone(),
            practitioner_id: "Concurrent downstream test practitioner".to_owned(),
            reason: "Record exact reconciliation evidence before concurrent reads.".to_owned(),
            idempotency_key: "concurrent-reconciliation-0001".to_owned(),
            local_readiness_only_acknowledged: true,
        })
        .expect("calculate exact reconciliation evidence");
    let candidate = reconciliation
        .create_candidate(&CreateCandidateRequest {
            schema_version: "migration-stream/write/create-result-candidate/v1".to_owned(),
            job_id: job_id.clone(),
            snapshot_id: snapshot.snapshot_id.clone(),
            snapshot_hash: snapshot.snapshot_hash.clone(),
            practitioner_id: "Concurrent downstream test practitioner".to_owned(),
            reason: "Prepare exact candidate evidence for concurrent read replay.".to_owned(),
            idempotency_key: "concurrent-candidate-0000001".to_owned(),
            local_readiness_only_acknowledged: true,
        })
        .expect("create exact candidate evidence");

    let concurrent_request_count = 32_usize;
    let barrier = Arc::new(Barrier::new(concurrent_request_count + 1));
    let mut concurrent_reads = Vec::with_capacity(concurrent_request_count);
    for index in 0..concurrent_request_count {
        let app = prevalidation_reopened_app.clone();
        let barrier = Arc::clone(&barrier);
        concurrent_reads.push(tokio::spawn(async move {
            barrier.wait().await;
            if index % 2 == 0 {
                (
                    "snapshot",
                    get_json(&app, "/api/v1/result-reconciliation/snapshots?page_size=50").await,
                )
            } else {
                (
                    "candidate",
                    get_json(&app, "/api/v1/result-candidates?page_size=50").await,
                )
            }
        }));
    }
    barrier.wait().await;
    for concurrent_read in concurrent_reads {
        let (kind, response) = concurrent_read.await.expect("join concurrent read");
        assert_eq!(
            response.status,
            StatusCode::OK,
            "{kind}: {:?}",
            response.body
        );
        match kind {
            "snapshot" => {
                assert_eq!(
                    response.body["items"][0]["snapshot_id"],
                    snapshot.snapshot_id
                );
                assert_eq!(
                    response.body["items"][0]["snapshot_hash"],
                    snapshot.snapshot_hash
                );
            }
            "candidate" => {
                assert_eq!(
                    response.body["items"][0]["candidate_id"],
                    candidate.candidate_id
                );
                assert_eq!(
                    response.body["items"][0]["candidate_hash"],
                    candidate.candidate_hash
                );
            }
            _ => unreachable!("bounded concurrent read kind"),
        }
    }

    // A cached downstream router must not repeat the direct Workbench writer
    // preflight on every React read. Holding that exact exclusive preflight
    // lock makes the old rebuild-per-request behavior fail deterministically,
    // while the cached stateless read services remain safe and value-free.
    let held_writer_lock = fs::File::open(fixture.workspace.join(".migration-stream.lock"))
        .expect("open exact direct Workbench preflight lock");
    held_writer_lock
        .try_lock_exclusive()
        .expect("hold exact direct Workbench preflight lock");
    let cached_request_count = 16_usize;
    let cached_barrier = Arc::new(Barrier::new(cached_request_count + 1));
    let mut cached_reads = Vec::with_capacity(cached_request_count);
    for index in 0..cached_request_count {
        let app = prevalidation_reopened_app.clone();
        let barrier = Arc::clone(&cached_barrier);
        cached_reads.push(tokio::spawn(async move {
            barrier.wait().await;
            let uri = if index % 2 == 0 {
                "/api/v1/result-reconciliation/snapshots?page_size=50"
            } else {
                "/api/v1/result-candidates?page_size=50"
            };
            get_json(&app, uri).await
        }));
    }
    cached_barrier.wait().await;
    for cached_read in cached_reads {
        let response = cached_read.await.expect("join cached concurrent read");
        assert_eq!(response.status, StatusCode::OK, "{:?}", response.body);
    }
    fs2::FileExt::unlock(&held_writer_lock).expect("release direct Workbench preflight lock");

    let handoff_context = get_json(&prevalidation_reopened_app, "/api/v1/context").await;
    assert_eq!(
        handoff_context.status,
        StatusCode::OK,
        "{:?}",
        handoff_context.body
    );
    assert_eq!(
        handoff_context.body["schema_version"],
        "migration-stream/workbench/context/v4"
    );
    assert_eq!(
        handoff_context.body["capabilities"]["governed_issue_decisions"],
        true
    );
    assert_eq!(
        handoff_context.body["capabilities"]["migration_result_approval"],
        true
    );
    assert_eq!(handoff_context.body["capabilities"]["publication"], true);
    assert_eq!(handoff_context.body["capabilities"]["target_write"], false);
    let handoff_journey = get_json(&prevalidation_reopened_app, "/api/v1/journey").await;
    assert_eq!(handoff_journey.status, StatusCode::OK);
    assert_eq!(handoff_journey.body["currentStageId"], "DECISIONS");
    assert_eq!(handoff_journey.body["binding"]["jobId"], job_id);
    assert_eq!(
        handoff_journey.body["binding"]["validationRunId"],
        launched.body["status"]["runId"]
    );
    assert_eq!(handoff_journey.body["stages"][0]["href"], "/");
    assert_eq!(handoff_journey.body["stages"][1]["href"], "/profile");
    assert_eq!(handoff_journey.body["stages"][2]["href"], "/files");
    assert_eq!(handoff_journey.body["stages"][5]["status"], "AVAILABLE");
    assert_eq!(handoff_journey.body["stages"][5]["href"], "/comparison");
    assert_eq!(handoff_journey.body["stages"][7]["href"], "/decisions");
    assert_eq!(handoff_journey.body["stages"][8]["href"], "/reconciliation");
    assert_eq!(handoff_journey.body["stages"][9]["href"], "/publication");

    let issues = get_json(&prevalidation_reopened_app, "/api/v1/issues?page_size=100").await;
    assert_eq!(issues.status, StatusCode::OK, "{:?}", issues.body);
    assert_eq!(
        issues.body["schema_version"],
        "migration-stream/read/issue-list/v1"
    );
    assert!(
        issues.body["items"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );

    let baseline_remediation =
        exercise_remediation_pack_http(&prevalidation_reopened_app, &fixture).await;
    assert_global_integrity(&fixture.workspace, "remediation-pack HTTP integrity replay");

    close_validation_issues_for_local_readiness(&fixture.workspace, &job_id, &validation_run_id);
    let downstream = ReconciliationApprovalService::new(&fixture.workspace);
    let readiness_snapshot = downstream
        .calculate(&CalculateReconciliationRequest {
            schema_version: "migration-stream/write/calculate-reconciliation/v1".to_owned(),
            job_id: job_id.clone(),
            validation_run_id: validation_run_id.clone(),
            practitioner_id: "Synthetic revision readiness practitioner".to_owned(),
            reason: "Close the exact baseline run before proving immutable revision history."
                .to_owned(),
            idempotency_key: "fresh-revision-readiness-calculate-0001".to_owned(),
            local_readiness_only_acknowledged: true,
        })
        .expect("calculate baseline readiness evidence");
    assert_eq!(readiness_snapshot.blocked_count, 0);
    let readiness_candidate = downstream
        .create_candidate(&CreateCandidateRequest {
            schema_version: "migration-stream/write/create-result-candidate/v1".to_owned(),
            job_id: job_id.clone(),
            snapshot_id: readiness_snapshot.snapshot_id.clone(),
            snapshot_hash: readiness_snapshot.snapshot_hash.clone(),
            practitioner_id: "Synthetic revision readiness practitioner".to_owned(),
            reason: "Prepare one exact baseline candidate before accepting a revised dump."
                .to_owned(),
            idempotency_key: "fresh-revision-readiness-candidate-0001".to_owned(),
            local_readiness_only_acknowledged: true,
        })
        .expect("create baseline readiness candidate");
    assert!(readiness_candidate.eligible);
    let required_warning_acknowledgements: Vec<String> = {
        let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
            .expect("open baseline candidate evidence");
        let canonical: String = connection
            .query_row(
                "SELECT required_acknowledgments_json
                 FROM migration_result_candidates WHERE candidate_id = ?1",
                [&readiness_candidate.candidate_id],
                |row| row.get(0),
            )
            .expect("read exact baseline candidate warning acknowledgements");
        serde_json::from_str(&canonical)
            .expect("parse exact baseline candidate warning acknowledgements")
    };
    let readiness_approval = downstream
        .apply_action(
            &job_id,
            &ResultActionPayload {
                schema_version: RECONCILIATION_SCHEMA_VERSION,
                kind: ResultActionKind::Approved,
                candidate_id: readiness_candidate.candidate_id.clone(),
                candidate_hash: readiness_candidate.candidate_hash.clone(),
                snapshot_id: readiness_snapshot.snapshot_id.clone(),
                snapshot_hash: readiness_snapshot.snapshot_hash.clone(),
                actor: "Synthetic revision readiness approver".to_owned(),
                reason: "Approve the exact local-only baseline readiness candidate.".to_owned(),
                warning_acknowledgements: required_warning_acknowledgements,
                authority_boundary_acknowledged: true,
                authority_boundary_acknowledgement_version: "migration-stream-result-boundary/v1"
                    .to_owned(),
                predecessor_action_id: None,
                predecessor_action_hash: None,
                observed_generation: 0,
                idempotency_key: "fresh-revision-readiness-approve-0001".to_owned(),
            },
        )
        .expect("approve baseline readiness candidate");
    let eligibility = PublicationReadService::new(&fixture.workspace)
        .get_eligibility(&job_id)
        .expect("read baseline publication eligibility");
    assert!(eligibility.is_eligible);
    assert_eq!(
        eligibility.approval_id.as_deref(),
        Some(readiness_approval.action_id.as_str())
    );
    let baseline_publication = PublicationService::new(&fixture.workspace)
        .publish(&PublicationRequest {
            schema_version: PUBLICATION_COMMAND_SCHEMA.to_owned(),
            job_id: job_id.clone(),
            candidate_id: readiness_candidate.candidate_id.clone(),
            approval_id: readiness_approval.action_id,
            practitioner: "Synthetic revision readiness publisher".to_owned(),
            publication_reason:
                "Preserve the exact local baseline package while later source revisions advance."
                    .to_owned(),
            acknowledgments: eligibility.required_acknowledgments,
            idempotency_key: "fresh-revision-readiness-publication-0001".to_owned(),
        })
        .expect("publish baseline readiness package");
    let engagement = get_json(&prevalidation_reopened_app, "/api/v1/engagement").await;
    let engagement_id = engagement.body["state"]["engagementId"]
        .as_str()
        .expect("baseline engagement ID")
        .to_owned();
    let (_, baseline_publication_bytes) = PublicationReadService::new(&fixture.workspace)
        .read_archive_for_engagement(&engagement_id, &baseline_publication.publication_id)
        .expect("read scoped baseline readiness package")
        .expect("baseline readiness package belongs to engagement");
    assert_eq!(
        migration_stream::canonical::lower_hex(Sha256::digest(&baseline_publication_bytes)),
        baseline_publication.physical_archive_digest
    );
    let baseline_downstream_evidence = immutable_downstream_evidence(&fixture.workspace, &job_id);
    for (label, rows) in &baseline_downstream_evidence {
        if matches!(
            label.as_str(),
            "correction overlays" | "correction effectivations"
        ) {
            assert!(rows.is_empty(), "accepted-as-is decisions created {label}");
        } else {
            assert!(!rows.is_empty(), "baseline proof omitted {label}");
        }
    }

    let revision_two_csv = concat!(
        "migrationPartnerId,category,organizationName1,notes\n",
        "CUST-003,1,",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ",Over length retained\n",
        "CUST-002,2,,\n",
        "CUST-001,2,Recovered Organization,Missing identifier fixed\n"
    );
    let (revision_two, revision_two_chain) = complete_source_revision_http(
        &prevalidation_reopened_app,
        "fresh-revision-two",
        revision_two_csv,
        Some(&fixture.workspace),
    )
    .await;
    assert_eq!(revision_two["activeTour"]["tourNumber"], 2);
    assert!(
        revision_two["comparison"]["closedCount"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(
        revision_two["comparison"]["remainingCount"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    assert!(
        revision_two["comparison"]["newCount"]
            .as_u64()
            .is_some_and(|count| count > 0)
    );
    let revision_two_context = get_json(&prevalidation_reopened_app, "/api/v1/context").await;
    assert_eq!(revision_two_context.status, StatusCode::OK);
    let revision_two_job = revision_two_context.body["job_id"]
        .as_str()
        .expect("Revision 2 job")
        .to_owned();
    assert_ne!(revision_two_job, job_id);

    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("inspect fresh downstream revision authority");
    for (table, column) in [
        ("issue_decisions", "job_id"),
        ("reconciliation_snapshots", "job_id"),
        ("migration_result_candidates", "job_id"),
        ("migration_result_actions", "job_id"),
        ("publication_records", "job_id"),
    ] {
        let count: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
                [&revision_two_job],
                |row| row.get(0),
            )
            .expect("count revision-scoped downstream evidence");
        assert_eq!(count, 0, "Revision 2 inherited {table} authority");
    }
    drop(connection);

    assert_eq!(revision_two_context.body["job_id"], revision_two_job);
    let revision_two_run = revision_two_chain["activeTour"]["validationRunId"]
        .as_str()
        .expect("Revision 2 validation run")
        .to_owned();
    let revision_two_queue = get_json(
        &prevalidation_reopened_app,
        "/api/v1/decision-queue?page_size=100",
    )
    .await;
    assert_eq!(revision_two_queue.status, StatusCode::OK);
    assert_eq!(revision_two_queue.body["scope"]["job_id"], revision_two_job);
    assert_eq!(
        revision_two_queue.body["scope"]["validation_run_id"],
        revision_two_run
    );
    assert!(
        revision_two_queue.body["items"]
            .as_array()
            .is_some_and(|items| {
                !items.is_empty() && items.iter().all(|item| item["active_decision"].is_null())
            })
    );
    for (uri, expected_schema) in [
        (
            "/api/v1/result-reconciliation/snapshots?page_size=100",
            "migration-stream/read/result-reconciliation-snapshot-list/v1",
        ),
        (
            "/api/v1/result-candidates?page_size=100",
            "migration-stream/read/result-candidate-list/v1",
        ),
    ] {
        let response = get_json(&prevalidation_reopened_app, uri).await;
        assert_eq!(
            response.status,
            StatusCode::OK,
            "{uri}: {:?}",
            response.body
        );
        assert_eq!(response.body["schema_version"], expected_schema, "{uri}");
        assert_eq!(response.body["scope"]["job_id"], revision_two_job, "{uri}");
        assert!(
            response.body["items"].as_array().is_some_and(Vec::is_empty),
            "{uri} inherited prior-run authority: {:?}",
            response.body
        );
    }
    let revision_two_publications =
        get_json(&prevalidation_reopened_app, "/api/v1/publications").await;
    assert_eq!(revision_two_publications.status, StatusCode::OK);
    assert_eq!(revision_two_publications.body["job_id"], revision_two_job);
    assert_eq!(revision_two_publications.body["total_count"], 0);
    assert!(
        revision_two_publications.body["items"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );

    let historical_publications = get_json(
        &prevalidation_reopened_app,
        "/api/v1/engagement/readiness-packages",
    )
    .await;
    assert_eq!(historical_publications.status, StatusCode::OK);
    assert!(
        historical_publications.body["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| {
                item["publication_id"] == baseline_publication.publication_id
                    && item["job_id"] == job_id
                    && item["physical_archive_digest"]
                        == baseline_publication.physical_archive_digest
            }))
    );
    let (historical_status, historical_bytes) = get_bytes(
        &prevalidation_reopened_app,
        &format!(
            "/api/v1/engagement/readiness-packages/{}/download",
            baseline_publication.publication_id
        ),
    )
    .await;
    assert_eq!(historical_status, StatusCode::OK);
    assert_eq!(historical_bytes, baseline_publication_bytes);
    let (revision_two_remediation_options, revision_two_remediation_pack, revision_two_zip) =
        create_active_remediation_pack(
            &prevalidation_reopened_app,
            "fresh-revision-two-remediation",
            false,
        )
        .await;
    assert_eq!(
        revision_two_remediation_options["validationRunId"],
        revision_two_run
    );
    assert_eq!(
        revision_two_remediation_pack["includeOffendingValues"],
        false
    );
    let revision_two_payload = String::from_utf8(remediation_zip_member(
        &revision_two_zip,
        "remediation-issues.json",
    ))
    .expect("Revision 2 remediation payload UTF-8");
    assert!(revision_two_payload.contains("CUST-003"));
    assert!(
        !revision_two_payload
            .contains("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
    );

    let original_retry_after_two = command_json(
        &prevalidation_reopened_app,
        Method::POST,
        "/api/v1/engagement/remediation-packs",
        "engagement-remediation-pack-create-v1",
        "fresh-remediation-keys-only-0001",
        baseline_remediation.keys_only_command.clone(),
    )
    .await;
    assert_eq!(
        original_retry_after_two.status,
        StatusCode::OK,
        "{:?}",
        original_retry_after_two.body
    );
    assert_eq!(original_retry_after_two.body["reusedAttempt"], true);
    assert_eq!(
        original_retry_after_two.body["pack"],
        baseline_remediation.keys_only_pack
    );
    let revision_two_remediation_history = get_json(
        &prevalidation_reopened_app,
        "/api/v1/engagement/remediation-packs",
    )
    .await;
    assert_eq!(revision_two_remediation_history.status, StatusCode::OK);
    assert_eq!(revision_two_remediation_history.body["totalCount"], 3);
    assert_history_contains_exact(
        &revision_two_remediation_history.body,
        &baseline_remediation.history,
    );
    assert_eq!(
        revision_two_remediation_history.body["items"]
            .as_array()
            .expect("Revision 2 remediation history")
            .iter()
            .filter(|item| { item["packId"] == baseline_remediation.keys_only_pack["packId"] })
            .count(),
        1,
        "replaying the original Revision 1 command inflated history"
    );
    for (pack_id, expected_bytes) in &baseline_remediation.archives {
        let (status, bytes) = get_bytes(
            &prevalidation_reopened_app,
            &format!("/api/v1/engagement/remediation-packs/{pack_id}/download"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&bytes, expected_bytes);
    }

    let revision_three_csv = concat!(
        "migrationPartnerId,category,organizationName1,notes\n",
        "CUST-001,2,Recovered Organization,Missing identifier fixed\n",
        "CUST-003,1,",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ",Over length retained\n",
        "CUST-002,2,Second Organization,\n"
    );
    let (revision_three, revision_three_chain) = complete_source_revision_http(
        &prevalidation_reopened_app,
        "fresh-revision-three",
        revision_three_csv,
        None,
    )
    .await;
    assert_eq!(revision_three["activeTour"]["tourNumber"], 3);
    assert_eq!(
        revision_three_chain["tours"].as_array().map(Vec::len),
        Some(3)
    );
    assert_eq!(
        revision_three_chain["comparisons"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        revision_three["comparison"]["closedCount"], 1,
        "Revision 3 should close the newly introduced conditional-required finding"
    );
    let revision_three_context = get_json(&prevalidation_reopened_app, "/api/v1/context").await;
    assert_eq!(revision_three_context.status, StatusCode::OK);
    let revision_three_job = revision_three_context.body["job_id"]
        .as_str()
        .expect("Revision 3 job")
        .to_owned();
    assert_ne!(revision_three_job, revision_two_job);
    let connection = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("inspect Revision 3 downstream authority");
    for (table, column) in [
        ("issue_decisions", "job_id"),
        ("reconciliation_snapshots", "job_id"),
        ("migration_result_candidates", "job_id"),
        ("migration_result_actions", "job_id"),
        ("publication_records", "job_id"),
    ] {
        let count: i64 = connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
                [&revision_three_job],
                |row| row.get(0),
            )
            .expect("count Revision 3 downstream evidence");
        assert_eq!(count, 0, "Revision 3 inherited {table} authority");
    }
    drop(connection);

    let (revision_three_key_options, revision_three_keys_pack, revision_three_keys_zip) =
        create_active_remediation_pack(
            &prevalidation_reopened_app,
            "fresh-revision-three-remediation",
            false,
        )
        .await;
    let (revision_three_value_options, revision_three_values_pack, revision_three_values_zip) =
        create_active_remediation_pack(
            &prevalidation_reopened_app,
            "fresh-revision-three-remediation",
            true,
        )
        .await;
    assert_eq!(
        revision_three_key_options["validationRunId"],
        revision_three_chain["activeTour"]["validationRunId"]
    );
    assert_eq!(revision_three_value_options, revision_three_key_options);
    assert_ne!(
        revision_three_keys_pack["packId"],
        revision_three_values_pack["packId"]
    );
    let revision_three_keys_payload = String::from_utf8(remediation_zip_member(
        &revision_three_keys_zip,
        "remediation-issues.json",
    ))
    .expect("Revision 3 keys-only remediation payload UTF-8");
    let revision_three_values_payload = String::from_utf8(remediation_zip_member(
        &revision_three_values_zip,
        "remediation-issues.json",
    ))
    .expect("Revision 3 offending-values remediation payload UTF-8");
    let revision_three_offending_value =
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    assert!(!revision_three_keys_payload.contains(revision_three_offending_value));
    assert!(revision_three_values_payload.contains(revision_three_offending_value));

    let original_retry_after_three = command_json(
        &prevalidation_reopened_app,
        Method::POST,
        "/api/v1/engagement/remediation-packs",
        "engagement-remediation-pack-create-v1",
        "fresh-remediation-keys-only-0001",
        baseline_remediation.keys_only_command.clone(),
    )
    .await;
    assert_eq!(
        original_retry_after_three.status,
        StatusCode::OK,
        "{:?}",
        original_retry_after_three.body
    );
    assert_eq!(original_retry_after_three.body["reusedAttempt"], true);
    assert_eq!(
        original_retry_after_three.body["pack"],
        baseline_remediation.keys_only_pack
    );
    let final_remediation_history = get_json(
        &prevalidation_reopened_app,
        "/api/v1/engagement/remediation-packs",
    )
    .await;
    assert_eq!(final_remediation_history.status, StatusCode::OK);
    assert_eq!(final_remediation_history.body["totalCount"], 5);
    assert_history_contains_exact(
        &final_remediation_history.body,
        &revision_two_remediation_history.body,
    );
    assert_eq!(
        final_remediation_history.body["items"]
            .as_array()
            .expect("Revision 3 remediation history")
            .iter()
            .filter(|item| { item["packId"] == baseline_remediation.keys_only_pack["packId"] })
            .count(),
        1,
        "replaying the original Revision 1 command after Revision 3 inflated history"
    );
    for received in [
        "CUST-001",
        "CUST-002",
        "CUST-003",
        revision_three_offending_value,
    ] {
        assert!(
            !final_remediation_history
                .body
                .to_string()
                .contains(received),
            "remediation history leaked received value {received}"
        );
    }
    let mut all_remediation_archives = baseline_remediation.archives.clone();
    all_remediation_archives.push((
        revision_two_remediation_pack["packId"]
            .as_str()
            .expect("Revision 2 remediation pack ID")
            .to_owned(),
        revision_two_zip,
    ));
    all_remediation_archives.push((
        revision_three_keys_pack["packId"]
            .as_str()
            .expect("Revision 3 keys-only remediation pack ID")
            .to_owned(),
        revision_three_keys_zip,
    ));
    all_remediation_archives.push((
        revision_three_values_pack["packId"]
            .as_str()
            .expect("Revision 3 offending-values remediation pack ID")
            .to_owned(),
        revision_three_values_zip,
    ));

    let revision_reopened = application(&fixture);
    let reopened_chain = get_json(&revision_reopened, "/api/v1/engagement/tours").await;
    assert_eq!(reopened_chain.status, StatusCode::OK);
    assert_eq!(reopened_chain.body, revision_three_chain);
    let reopened_remediation =
        get_json(&revision_reopened, "/api/v1/engagement/remediation-packs").await;
    assert_eq!(reopened_remediation.body, final_remediation_history.body);
    for (pack_id, expected_bytes) in &all_remediation_archives {
        let (status, bytes) = get_bytes(
            &revision_reopened,
            &format!("/api/v1/engagement/remediation-packs/{pack_id}/download"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(&bytes, expected_bytes);
    }
    let (reopened_publication_status, reopened_publication_bytes) = get_bytes(
        &revision_reopened,
        &format!(
            "/api/v1/engagement/readiness-packages/{}/download",
            baseline_publication.publication_id
        ),
    )
    .await;
    assert_eq!(reopened_publication_status, StatusCode::OK);
    assert_eq!(reopened_publication_bytes, baseline_publication_bytes);
    assert_eq!(
        immutable_downstream_evidence(&fixture.workspace, &job_id),
        baseline_downstream_evidence,
        "later source revisions rewrote prior-run downstream evidence"
    );
    assert_global_integrity(
        &fixture.workspace,
        "three-revision single-entity integrity replay",
    );

    let reopened_app = application(&fixture);
    let reopened = command_json(
        &reopened_app,
        Method::POST,
        "/api/v1/engagement",
        "engagement-create-v1",
        "test-reopen-00000001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/create/v1",
            "displayName": "Unfamiliar sanitized readiness proof",
            "idempotencyKey": "test-reopen-00000001"
        }),
    )
    .await;
    assert_eq!(reopened.status, StatusCode::OK, "{:?}", reopened.body);
    assert_eq!(reopened.body["reopened"], true);
    let reopened_issues = get_json(&reopened_app, "/api/v1/issues?page_size=100").await;
    assert_eq!(reopened_issues.status, StatusCode::OK);
    assert_ne!(
        reopened_issues.body["canonical_hash"], issues.body["canonical_hash"],
        "the active issues read must advance to Revision 3 without rewriting baseline evidence"
    );
    assert_eq!(reopened_issues.body["scope"]["job_id"], revision_three_job);

    let database = Connection::open(fixture.workspace.join("state.sqlite3")).expect("database");
    database
        .execute(
            "UPDATE engagement_active_context
             SET validation_stage = 'NOT_LAUNCHED',
                 active_validation_run_id = NULL",
            [],
        )
        .expect("simulate stale engagement validation pointer");
    drop(database);
    let mismatched = get_json(&reopened_app, "/api/v1/issues?page_size=100").await;
    assert_eq!(mismatched.status, StatusCode::CONFLICT);
    assert_eq!(mismatched.body["error"]["code"], "INTEGRITY_FAILED");
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn unfamiliar_sales_mapping_survives_restart_and_stops_at_read_only_first_issues() {
    let fixture = sales_fixture("sales-document-golden");
    let app = application(&fixture);
    let (preflight_id, preflight) = create_and_preflight(
        &app,
        "Unfamiliar sales-document proof",
        "unfamiliar-sales.zip",
        sales_source_package(),
    )
    .await;
    assert_eq!(preflight["state"], "READY", "{preflight:?}");
    assert_eq!(
        preflight["manifest"]["csv_paths"],
        json!(["deal_book.csv", "deal_detail.csv"])
    );
    let source_hash = preflight["sourceHash"]
        .as_str()
        .expect("source hash")
        .to_owned();
    let manifest_hash = preflight["manifestHash"]
        .as_str()
        .expect("manifest hash")
        .to_owned();
    let accepted = command_json(
        &app,
        Method::POST,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/accept"),
        "engagement-source-accept-v1",
        "sales-accept-0000001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/accept-source/v1",
            "preflightId": preflight_id,
            "expectedSourceHash": source_hash,
            "expectedManifestHash": manifest_hash,
            "idempotencyKey": "sales-accept-0000001",
            "acknowledgement": "migration-stream/source-acceptance/v1"
        }),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{:?}", accepted.body);
    let job_id = accepted.body["jobId"].as_str().expect("job ID").to_owned();

    for uri in [
        "/api/v1/profiling?page_size=200",
        "/api/v1/profiling?include_samples=true&page_size=200",
    ] {
        let profiling = get_json(&app, uri).await;
        assert_eq!(profiling.status, StatusCode::OK, "{:?}", profiling.body);
        assert_eq!(profiling.body["items"].as_array().map(Vec::len), Some(14));
        assert!(
            profiling.body["items"]
                .as_array()
                .is_some_and(|items| items.iter().all(|item| {
                    item["sample_count"] == 0
                        && item
                            .get("samples")
                            .is_none_or(|samples| samples.as_array().is_some_and(Vec::is_empty))
                }))
        );
    }

    let selected = command_json(
        &app,
        Method::POST,
        "/api/v1/engagement/object-packs/select",
        "engagement-object-pack-select-v1",
        "sales-select-0000001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/select-object-pack/v1",
            "objectPackId": "synthetic.sales-document.readiness",
            "idempotencyKey": "sales-select-0000001"
        }),
    )
    .await;
    assert_eq!(selected.status, StatusCode::OK, "{:?}", selected.body);
    assert_eq!(
        selected.body["objectPack"]["mappingMode"],
        "OPERATOR_GOVERNED"
    );
    assert!(
        selected.body["objectPack"]["mappingSuggestions"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    let approved_profile = command_json(
        &app,
        Method::POST,
        "/api/v1/engagement/object-packs/approve",
        "engagement-profile-approval-v1",
        "sales-profile-approve1",
        json!({
            "schemaVersion": "migration-stream/engagement/write/approve-object-pack-profile/v1",
            "actor": "Synthetic local test practitioner",
            "reason": "Approve the exact target-neutral reviewed profile for local mapping and validation.",
            "authorityAcknowledgement": "migration-stream/local-mapping-validation-authority/v1",
            "idempotencyKey": "sales-profile-approve1"
        }),
    )
    .await;
    assert_eq!(
        approved_profile.status,
        StatusCode::OK,
        "{:?}",
        approved_profile.body
    );

    let workspace = get_json(&app, "/api/v1/engagement/mapping-workspace").await;
    assert_eq!(workspace.status, StatusCode::OK, "{:?}", workspace.body);
    assert_eq!(workspace.body["mappingMode"], "OPERATOR_GOVERNED");
    assert_eq!(
        workspace.body["sourceFiles"].as_array().map(|files| files
            .iter()
            .map(|file| file["sourceFile"].as_str().unwrap_or_default())
            .collect::<Vec<_>>()),
        Some(vec!["deal_book.csv", "deal_detail.csv"])
    );
    assert_eq!(workspace.body["entities"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        workspace.body["relationships"][0]["relationshipId"],
        "sales-document.items"
    );
    let workspace_generation = workspace.body["engagementGeneration"]
        .as_u64()
        .expect("engagement generation");

    let save_body = json!({
        "schemaVersion": "migration-stream/engagement/write/save-mapping-draft/v1",
        "idempotencyKey": "sales-save-000000001",
        "expectedEngagementGeneration": workspace_generation,
        "expectedDraftId": null,
        "expectedDraftGeneration": null,
        "expectedDraftHash": null,
        "snapshot": sales_mapping_snapshot()
    });
    let saved = command_json(
        &app,
        Method::POST,
        "/api/v1/engagement/mapping-drafts/save",
        "engagement-mapping-draft-save-v1",
        "sales-save-000000001",
        save_body.clone(),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{:?}", saved.body);
    assert_eq!(saved.body["draft"]["state"], "EDITABLE");
    let saved_replay = command_json(
        &app,
        Method::POST,
        "/api/v1/engagement/mapping-drafts/save",
        "engagement-mapping-draft-save-v1",
        "sales-save-000000001",
        save_body,
    )
    .await;
    assert_eq!(
        saved_replay.status,
        StatusCode::OK,
        "{:?}",
        saved_replay.body
    );
    assert_eq!(saved_replay.body["idempotentReplay"], true);
    assert_eq!(
        saved_replay.body["draft"]["draftHash"],
        saved.body["draft"]["draftHash"]
    );

    let restarted = application(&fixture);
    let reloaded = get_json(&restarted, "/api/v1/engagement/mapping-workspace").await;
    assert_eq!(reloaded.status, StatusCode::OK, "{:?}", reloaded.body);
    assert_eq!(reloaded.body["draft"]["snapshot"], sales_mapping_snapshot());
    let submitted = command_json(
        &restarted,
        Method::POST,
        "/api/v1/engagement/mapping-drafts/submit",
        "engagement-mapping-draft-submit-v1",
        "sales-submit-0000001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/submit-mapping-draft/v1",
            "idempotencyKey": "sales-submit-0000001",
            "expectedEngagementGeneration": reloaded.body["engagementGeneration"],
            "draftId": reloaded.body["draft"]["draftId"],
            "expectedDraftGeneration": reloaded.body["draft"]["draftGeneration"],
            "expectedDraftHash": reloaded.body["draft"]["draftHash"],
            "mappingId": "synthetic.sales-document.http",
            "version": "1.0.0",
            "acknowledgement": "migration-stream/operator-governed-mapping-reviewed-and-frozen/v1"
        }),
    )
    .await;
    assert_eq!(submitted.status, StatusCode::OK, "{:?}", submitted.body);
    assert_eq!(submitted.body["draft"]["state"], "SUBMITTED");
    assert!(
        submitted.body["coverage"]["requiredCanonicalFieldsWithoutCoverage"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert!(
        submitted.body["coverage"]["invalidBindings"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    let mapping_hash = submitted.body["mappingHash"]
        .as_str()
        .expect("mapping hash")
        .to_owned();
    let submitted_coverage = get_json(
        &restarted,
        "/api/v1/engagement/mapping-coverage?page_size=200",
    )
    .await;
    assert_eq!(
        submitted_coverage.status,
        StatusCode::OK,
        "{:?}",
        submitted_coverage.body
    );
    assert_eq!(
        submitted_coverage.body["scope"]["mapping_hash"],
        mapping_hash
    );

    let restarted_again = application(&fixture);
    let durable_coverage = get_json(
        &restarted_again,
        "/api/v1/engagement/mapping-coverage?page_size=200",
    )
    .await;
    assert_eq!(
        durable_coverage.status,
        StatusCode::OK,
        "{:?}",
        durable_coverage.body
    );
    assert_eq!(
        durable_coverage.body["canonical_hash"],
        submitted_coverage.body["canonical_hash"]
    );
    let submitted_workspace =
        get_json(&restarted_again, "/api/v1/engagement/mapping-workspace").await;
    let mapping_approval = command_json(
        &restarted_again,
        Method::POST,
        "/api/v1/engagement/mappings/approve",
        "engagement-mapping-version-approve-v1",
        "sales-mapping-approve1",
        json!({
            "schemaVersion": "migration-stream/engagement/write/approve-mapping-version/v1",
            "idempotencyKey": "sales-mapping-approve1",
            "expectedEngagementGeneration": submitted_workspace.body["engagementGeneration"],
            "draftId": submitted_workspace.body["draft"]["draftId"],
            "expectedDraftGeneration": submitted_workspace.body["draft"]["draftGeneration"],
            "expectedDraftHash": submitted_workspace.body["draft"]["draftHash"],
            "expectedMappingId": submitted_workspace.body["draft"]["submittedMappingId"],
            "expectedMappingVersion": submitted_workspace.body["draft"]["submittedMappingVersion"],
            "expectedMappingHash": submitted_workspace.body["draft"]["submittedMappingHash"],
            "actor": "Synthetic local test practitioner",
            "reason": "Approve the exact frozen two-entity mapping hash for local validation.",
            "acknowledgement": "migration-stream/local-mapping-version-approval/v1"
        }),
    )
    .await;
    assert_eq!(
        mapping_approval.status,
        StatusCode::OK,
        "{:?}",
        mapping_approval.body
    );
    assert_eq!(mapping_approval.body["mappingHash"], mapping_hash);

    let launched = command_json(
        &restarted_again,
        Method::POST,
        "/api/v1/engagement/validation/launch",
        "engagement-validation-launch-v1",
        "sales-validation-0001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/launch-validation/v1",
            "idempotencyKey": "sales-validation-0001",
            "expectedEngagementGeneration": mapping_approval.body["engagementGeneration"],
            "expectedJobId": job_id,
            "expectedMappingId": mapping_approval.body["mappingId"],
            "expectedMappingVersion": mapping_approval.body["mappingVersion"],
            "expectedMappingHash": mapping_approval.body["mappingHash"]
        }),
    )
    .await;
    assert_eq!(launched.status, StatusCode::OK, "{:?}", launched.body);
    assert_eq!(launched.body["status"]["rowsProcessed"], 10);
    assert_eq!(launched.body["status"]["issueCount"], 5);

    let context = get_json(&restarted_again, "/api/v1/context").await;
    assert_eq!(context.status, StatusCode::OK, "{:?}", context.body);
    assert_eq!(
        context.body["schemaVersion"],
        "migration-stream/engagement/context/v1"
    );
    assert_eq!(
        context.body["capabilities"]["governedIssueDecisions"],
        false
    );
    assert_eq!(
        context.body["capabilities"]["migrationResultApproval"],
        false
    );
    assert_eq!(context.body["capabilities"]["publication"], false);
    assert_eq!(context.body["capabilities"]["targetWrite"], false);
    let multi_journey = get_json(&restarted_again, "/api/v1/journey").await;
    assert_eq!(multi_journey.status, StatusCode::OK);
    assert_eq!(multi_journey.body["currentStageId"], "ISSUES");
    assert_eq!(multi_journey.body["binding"]["entityCount"], 2);
    assert_eq!(multi_journey.body["stages"][5]["status"], "AVAILABLE");
    assert_eq!(multi_journey.body["stages"][5]["href"], "/comparison");
    for index in 7..=9 {
        assert_eq!(multi_journey.body["stages"][index]["status"], "LOCKED");
        assert_eq!(multi_journey.body["stages"][index]["href"], Value::Null);
        assert_eq!(
            multi_journey.body["stages"][index]["unavailableReason"],
            "Multi-entity downstream authority is not implemented yet."
        );
    }

    let issues = get_json(&restarted_again, "/api/v1/issues?page_size=200").await;
    assert_eq!(issues.status, StatusCode::OK, "{:?}", issues.body);
    assert_eq!(issues.body["items"].as_array().map(Vec::len), Some(5));
    let issue_codes = issues.body["items"]
        .as_array()
        .expect("issue items")
        .iter()
        .filter_map(|item| item["issue_code"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        issue_codes,
        std::collections::BTreeSet::from([
            "PATTERN_MISMATCH",
            "RELATIONSHIP_MINIMUM_CHILD_MISSING",
            "RELATIONSHIP_PARENT_NOT_FOUND",
            "UNIQUE_DUPLICATE",
        ])
    );
    let broad_json = issues.body.to_string();
    for received_value in ["SD-1001", "CUST-0042", "SKU-A", "PO-7781"] {
        assert!(!broad_json.contains(received_value));
    }
}

#[tokio::test]
async fn preflight_reservation_commits_file_intent_before_create_and_rolls_back_cleanly() {
    let fixture = fixture("preflight-db-first-failure");
    let app = application(&fixture);
    let created = command_json(
        &app,
        Method::POST,
        "/api/v1/engagement",
        "engagement-create-v1",
        "db-first-create-0001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/create/v1",
            "displayName": "DB-first source reservation",
            "idempotencyKey": "db-first-create-0001"
        }),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open workspace database");
    connection
        .execute_batch(
            "CREATE TRIGGER reject_test_preflight_event
             BEFORE INSERT ON engagement_preflight_events
             BEGIN
                 SELECT RAISE(ABORT, 'synthetic event persistence failure');
             END;",
        )
        .expect("install deterministic event failure");
    drop(connection);

    let reserve = command_json(
        &app,
        Method::POST,
        "/api/v1/engagement/source-preflights",
        "engagement-source-preflight-reserve-v1",
        "db-first-reserve-0001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/reserve-source-preflight/v1",
            "sourceName": "synthetic.zip",
            "idempotencyKey": "db-first-reserve-0001"
        }),
    )
    .await;
    assert_eq!(reserve.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        !reserve
            .body
            .to_string()
            .contains(fixture.workspace.to_string_lossy().as_ref()),
        "HTTP failures must not expose a local workspace path"
    );
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("reopen database");
    let rows: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_source_preflights",
            [],
            |row| row.get(0),
        )
        .expect("count rolled-back preflights");
    assert_eq!(rows, 0);
    assert!(
        !fixture.workspace.join("engagement/preflights").exists(),
        "no file namespace may precede its committed RECEIVING intent"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn upload_rejects_external_hardlink_alias_without_touching_its_bytes() {
    let fixture = fixture("preflight-hardlink-alias");
    let app = application(&fixture);
    let reserve_key = "hardlink-reserve-0001";
    let (preflight_id, _) = create_and_reserve(
        &app,
        "Hard-link upload boundary",
        "synthetic.zip",
        "hardlink-create-0001",
        reserve_key,
    )
    .await;
    let reserved = fixture
        .workspace
        .join(format!("engagement/preflights/{preflight_id}.part"));
    let external = fixture
        .workspace
        .parent()
        .expect("fixture parent")
        .join("external-upload-sentinel.bin");
    fs::write(&external, b"").expect("create external sentinel");
    fs::remove_file(&reserved).expect("replace reserved name adversarially");
    fs::hard_link(&external, &reserved).expect("install external hard-link alias");

    let response = upload_zip(
        &app,
        &preflight_id,
        reserve_key,
        source_package("legacy_id,name\nC-1,Synthetic\n"),
    )
    .await;
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        fs::read(&external).expect("read external sentinel"),
        Vec::<u8>::new(),
        "upload must fail before writing through an external hard-link alias"
    );
    assert!(
        !response
            .body
            .to_string()
            .contains(fixture.workspace.to_string_lossy().as_ref())
    );
    let detail = get_json(&app, "/api/v1/engagement").await;
    let terminal = detail.body["preflights"]
        .as_array()
        .expect("preflight list")
        .iter()
        .find(|item| item["preflightId"] == preflight_id)
        .expect("terminal preflight");
    assert_eq!(terminal["state"], "REJECTED");
    assert_eq!(
        fs::read(&reserved).expect("mismatched replacement remains"),
        Vec::<u8>::new(),
        "exact cleanup must not unlink a mismatched replacement"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn preexisting_same_content_retained_hardlink_never_becomes_ready() {
    let fixture = fixture("preflight-retained-hardlink-injection");
    let app = application(&fixture);
    let reserve_key = "retained-injection-reserve-0001";
    let (preflight_id, _) = create_and_reserve(
        &app,
        "Retained hard-link injection boundary",
        "synthetic.zip",
        "retained-injection-create-0001",
        reserve_key,
    )
    .await;
    let bytes = source_package("legacy_id,name\nC-1,Synthetic\n");
    let source_hash = migration_stream::canonical::lower_hex(Sha256::digest(&bytes));
    let retained_directory = fixture.workspace.join("engagement/preflights/retained");
    fs::create_dir(&retained_directory).expect("create retained namespace");
    let retained = retained_directory.join(format!("{source_hash}.zip"));
    let external = fixture
        .workspace
        .parent()
        .expect("fixture parent")
        .join("external-same-content-package.zip");
    fs::write(&external, &bytes).expect("write same-content external package");
    let mut permissions = fs::metadata(&external)
        .expect("external package metadata")
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&external, permissions).expect("freeze external package");
    fs::hard_link(&external, &retained).expect("inject retained hard-link destination");

    let response = upload_zip(&app, &preflight_id, reserve_key, bytes.clone()).await;
    assert_eq!(response.status, StatusCode::CONFLICT);
    assert_eq!(response.body["error"]["code"], "INTEGRITY_FAILED");
    assert!(
        !response
            .body
            .to_string()
            .contains(fixture.workspace.to_string_lossy().as_ref())
    );
    assert_eq!(
        fs::read(&external).expect("read external package"),
        bytes,
        "failed publication must not mutate the injected external inode"
    );
    let detail = get_json(&app, "/api/v1/engagement").await;
    let preflight = detail.body["preflights"]
        .as_array()
        .expect("preflight list")
        .iter()
        .find(|item| item["preflightId"] == preflight_id)
        .expect("injected preflight");
    assert_ne!(
        preflight["state"], "READY",
        "foreign retained identity must never become READY authority"
    );
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open database");
    assert_unproven_retained_identity_not_blessed(&connection, &preflight_id);
    let ready_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_preflight_events
             WHERE preflight_id = ?1 AND state = 'READY'",
            [&preflight_id],
            |row| row.get(0),
        )
        .expect("count READY events");
    assert_eq!(ready_events, 0);
    drop(connection);
    assert!(
        retained.exists(),
        "foreign retained name must not be unlinked"
    );
    drop(app);
    let Err(error) = build_engagement_application_router(
        EngagementWorkbenchConfig {
            workspace: fixture.workspace.clone(),
            intake_policy: project_path("config/engagement-intake-policy.json"),
            profiling_policy: project_path("config/engagement-profiling-policy.json"),
            validation_policy: project_path("config/engagement-validation-policy.json"),
            object_packs: fixture.object_packs.clone(),
            profile_schema: project_path("schemas/migration-object-profile.schema.json"),
            mapping_schema: project_path("schemas/source-mapping-version.schema.json"),
        },
        &fixture.assets,
    ) else {
        panic!("recovery must reject a foreign retained-file identity");
    };
    assert!(matches!(error, KernelError::IntegrityFailed(_)));
}

#[tokio::test]
async fn generic_recovery_refuses_identity_bound_receiving_preflights() {
    let fixture = fixture("preflight-generic-recovery-boundary");
    let app = application(&fixture);
    let (preflight_id, _) = create_and_reserve(
        &app,
        "Identity-bound recovery boundary",
        "synthetic.zip",
        "recovery-boundary-create-0001",
        "recovery-boundary-reserve-0001",
    )
    .await;
    let reserved = fixture
        .workspace
        .join(format!("engagement/preflights/{preflight_id}.part"));
    fs::remove_file(&reserved).expect("remove exact reserved file");
    fs::write(&reserved, b"foreign replacement").expect("install foreign replacement");

    let error = recover_workspace(&fixture.workspace)
        .expect_err("generic recovery must defer engagement-owned file reconciliation");
    assert!(matches!(error, KernelError::Conflict(_)));
    assert!(
        error
            .to_string()
            .contains("identity-bound engagement service")
    );
    assert_eq!(
        fs::read(&reserved).expect("foreign replacement remains"),
        b"foreign replacement",
        "generic recovery must never delete a path-only replacement"
    );
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open database");
    let state: String = connection
        .query_row(
            "SELECT state FROM engagement_source_preflights WHERE preflight_id = ?1",
            [&preflight_id],
            |row| row.get(0),
        )
        .expect("read preflight state");
    assert_eq!(
        state, "RECEIVING",
        "generic recovery must not partially terminalize engagement authority"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn upload_cannot_escape_through_a_replaced_preflight_ancestor() {
    use std::os::unix::fs::symlink;

    let fixture = fixture("preflight-ancestor-replacement");
    let app = application(&fixture);
    let reserve_key = "ancestor-reserve-0001";
    let (preflight_id, _) = create_and_reserve(
        &app,
        "Ancestor replacement boundary",
        "synthetic.zip",
        "ancestor-create-0001",
        reserve_key,
    )
    .await;
    let engagement_directory = fixture.workspace.join("engagement");
    let preflights = engagement_directory.join("preflights");
    let displaced = engagement_directory.join("preflights-displaced");
    let external = fixture
        .workspace
        .parent()
        .expect("fixture parent")
        .join("external-preflight-target");
    fs::create_dir(&external).expect("create external target");
    fs::rename(&preflights, &displaced).expect("displace controlled ancestor");
    symlink(&external, &preflights).expect("replace ancestor with symlink");

    let response = upload_zip(
        &app,
        &preflight_id,
        reserve_key,
        source_package("legacy_id,name\nC-1,Synthetic\n"),
    )
    .await;
    assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        fs::read_dir(&external)
            .expect("read external target")
            .next()
            .is_none(),
        "no source bytes may be created beyond the retained workspace root"
    );
    assert!(
        !response
            .body
            .to_string()
            .contains(fixture.workspace.to_string_lossy().as_ref())
    );

    fs::remove_file(&preflights).expect("remove adversarial symlink");
    fs::rename(&displaced, &preflights).expect("restore controlled ancestor");
    let reopened = application_without_establishing_workspace(&fixture);
    let detail = get_json(&reopened, "/api/v1/engagement").await;
    let terminal = detail.body["preflights"]
        .as_array()
        .expect("preflight list")
        .iter()
        .find(|item| item["preflightId"] == preflight_id)
        .expect("terminal preflight");
    assert_eq!(terminal["state"], "REJECTED");
    assert!(
        !preflights.join(format!("{preflight_id}.part")).exists(),
        "recovery retries cleanup only after the exact controlled ancestor returns"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn publish_before_ready_is_replayed_exactly_once_after_restart() {
    let fixture = fixture("preflight-publish-replay");
    let app = application(&fixture);
    let (preflight_id, _) = create_and_reserve(
        &app,
        "Publish replay boundary",
        "synthetic.zip",
        "publish-create-0001",
        "publish-reserve-0001",
    )
    .await;
    let bytes = source_package("legacy_id,name\nC-1,Synthetic\n");
    let source_hash = migration_stream::canonical::lower_hex(Sha256::digest(&bytes));
    let source_size = u64::try_from(bytes.len()).expect("source size");
    let partial = fixture
        .workspace
        .join(format!("engagement/preflights/{preflight_id}.part"));
    fs::write(&partial, &bytes).expect("simulate synchronized browser upload");
    let policy = migration_stream::policy::IntakePolicy::from_path(Some(&project_path(
        "config/engagement-intake-policy.json",
    )))
    .expect("load intake policy");
    let manifest =
        migration_stream::intake::inspect_archive(&partial, &policy).expect("inspect source");
    let manifest_value = serde_json::to_value(manifest).expect("manifest JSON");
    let manifest_json = migration_stream::canonical::canonical_json_string(&manifest_value)
        .expect("canonical manifest");
    let manifest_hash =
        migration_stream::canonical::canonical_hash(&manifest_value).expect("manifest hash");
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open database");
    let file_intent: String = connection
        .query_row(
            "SELECT detail FROM engagement_preflight_events
             WHERE preflight_id = ?1 AND detail LIKE '%preflight-file-intent%'
             ORDER BY sequence DESC LIMIT 1",
            [&preflight_id],
            |row| row.get(0),
        )
        .expect("load durable file identity");
    let file_intent: Value = serde_json::from_str(&file_intent).expect("parse file intent");
    let publish_intent = json!({
        "schemaVersion": "migration-stream/engagement/preflight-publish-intent/v2",
        "retainedRelativePath": format!(
            "engagement/preflights/retained/{source_hash}.zip"
        ),
        "sourceHash": source_hash,
        "sourceSize": source_size,
        "manifestJson": manifest_json,
        "manifestHash": manifest_hash,
        "partialFileIdentity": file_intent["fileIdentity"].clone(),
        "retainedFileIdentity": file_intent["fileIdentity"].clone()
    });
    let publish_json = migration_stream::canonical::canonical_json_string(&publish_intent)
        .expect("canonical publish intent");
    connection
        .execute(
            "INSERT INTO engagement_preflight_events (
                 preflight_id, state, error_code, detail, occurred_at
             ) VALUES (?1, 'RECEIVING', NULL, ?2, '2026-07-29T00:00:00.000Z')",
            rusqlite::params![preflight_id, publish_json],
        )
        .expect("persist publish-before-READY intent");
    drop(connection);
    let retained_directory = fixture.workspace.join("engagement/preflights/retained");
    fs::create_dir(&retained_directory).expect("create retained namespace");
    let retained = retained_directory.join(format!("{source_hash}.zip"));
    let mut permissions = fs::metadata(&partial)
        .expect("partial metadata")
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&partial, permissions).expect("freeze uploaded source");
    fs::hard_link(&partial, &retained).expect("simulate published retained name");
    assert_global_integrity(
        &fixture.workspace,
        "published-before-READY global integrity",
    );

    let reopened = application_without_establishing_workspace(&fixture);
    let detail = get_json(&reopened, "/api/v1/engagement").await;
    let ready = detail.body["preflights"]
        .as_array()
        .expect("preflight list")
        .iter()
        .find(|item| item["preflightId"] == preflight_id)
        .expect("replayed preflight");
    assert_eq!(ready["state"], "READY");
    assert_eq!(ready["sourceHash"], source_hash);
    assert!(!partial.exists());
    assert!(retained.exists());

    let reopened_again = application_without_establishing_workspace(&fixture);
    let repeated = get_json(&reopened_again, "/api/v1/engagement").await;
    assert_eq!(repeated.status, StatusCode::OK);
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("reopen database");
    let ready_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_preflight_events
             WHERE preflight_id = ?1 AND state = 'READY'",
            [&preflight_id],
            |row| row.get(0),
        )
        .expect("count READY events");
    assert_eq!(ready_events, 1, "recovery must be exactly-once");
}

#[tokio::test]
async fn acceptance_intent_precedes_file_creation_and_insert_failure_leaves_no_file() {
    let fixture = fixture("acceptance-db-first-failure");
    let app = application(&fixture);
    let (preflight_id, ready) = create_and_preflight(
        &app,
        "Acceptance DB-first boundary",
        "synthetic.zip",
        source_package("legacy_id,name\nC-1,Synthetic\n"),
    )
    .await;
    assert_eq!(ready["state"], "READY", "{ready:?}");
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open database");
    connection
        .execute_batch(
            "CREATE TRIGGER reject_test_acceptance_attempt
             BEFORE INSERT ON engagement_acceptance_attempts
             BEGIN
                 SELECT RAISE(ABORT, 'synthetic acceptance persistence failure');
             END;",
        )
        .expect("install deterministic acceptance failure");
    drop(connection);
    let accept = command_json(
        &app,
        Method::POST,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/accept"),
        "engagement-source-accept-v1",
        "acceptance-db-first-0001",
        json!({
            "schemaVersion": "migration-stream/engagement/write/accept-source/v1",
            "preflightId": preflight_id,
            "expectedSourceHash": ready["sourceHash"],
            "expectedManifestHash": ready["manifestHash"],
            "idempotencyKey": "acceptance-db-first-0001",
            "acknowledgement": "migration-stream/source-acceptance/v1"
        }),
    )
    .await;
    assert_eq!(accept.status, StatusCode::INTERNAL_SERVER_ERROR);
    let acceptances = fixture.workspace.join("engagement/acceptances");
    assert!(
        !acceptances.exists()
            || fs::read_dir(&acceptances)
                .expect("read acceptance directory")
                .next()
                .is_none(),
        "no acceptance file may precede its durable attempt row"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn completed_acceptance_job_survives_binding_failure_and_same_key_restart_retry() {
    let fixture = fixture("acceptance-job-binding-retry");
    let app = application(&fixture);
    let bytes = source_package("legacy_id,name\nC-1,Synthetic\n");
    let (preflight_id, ready) =
        create_and_preflight(&app, "Acceptance job binding retry", "synthetic.zip", bytes).await;
    assert_eq!(ready["state"], "READY", "{ready:?}");
    let accept_body = json!({
        "schemaVersion": "migration-stream/engagement/write/accept-source/v1",
        "preflightId": preflight_id,
        "expectedSourceHash": ready["sourceHash"],
        "expectedManifestHash": ready["manifestHash"],
        "idempotencyKey": "acceptance-job-binding-retry-0001",
        "acknowledgement": "migration-stream/source-acceptance/v1"
    });
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open database");
    connection
        .execute_batch(
            "CREATE TRIGGER reject_test_acceptance_job_binding
             BEFORE UPDATE OF job_id ON engagement_acceptance_attempts
             WHEN OLD.job_id IS NULL AND NEW.job_id IS NOT NULL
             BEGIN
                 SELECT RAISE(ABORT, 'synthetic acceptance job binding failure');
             END;",
        )
        .expect("install deterministic post-intake failure");
    drop(connection);

    let failed = command_json(
        &app,
        Method::POST,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/accept"),
        "engagement-source-accept-v1",
        "acceptance-job-binding-retry-0001",
        accept_body.clone(),
    )
    .await;
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("inspect failed boundary");
    let attempt_job: Option<String> = connection
        .query_row(
            "SELECT job_id FROM engagement_acceptance_attempts
             WHERE idempotency_key = 'acceptance-job-binding-retry-0001'",
            [],
            |row| row.get(0),
        )
        .expect("acceptance intent remains unbound");
    let active_job: Option<String> = connection
        .query_row(
            "SELECT active_job_id FROM engagement_active_context",
            [],
            |row| row.get(0),
        )
        .expect("active source remains unset");
    let complete_jobs: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE state = 'COMPLETE'",
            [],
            |row| row.get(0),
        )
        .expect("count complete unbound jobs");
    assert!(attempt_job.is_none());
    assert!(
        active_job.is_none(),
        "failed binding must not poison active_job_id"
    );
    assert_eq!(complete_jobs, 1);
    connection
        .execute_batch("DROP TRIGGER reject_test_acceptance_job_binding;")
        .expect("remove deterministic fault");
    drop(connection);
    drop(app);

    let reopened = application_without_establishing_workspace(&fixture);
    let accepted = command_json(
        &reopened,
        Method::POST,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/accept"),
        "engagement-source-accept-v1",
        "acceptance-job-binding-retry-0001",
        accept_body,
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{:?}", accepted.body);
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("inspect recovered retry");
    let (jobs, bound_job, active_job): (i64, Option<String>, Option<String>) = connection
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM jobs),
                 (SELECT job_id FROM engagement_acceptance_attempts
                  WHERE idempotency_key = 'acceptance-job-binding-retry-0001'),
                 (SELECT active_job_id FROM engagement_active_context)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("load recovered acceptance authority");
    assert_eq!(jobs, 1, "same-key retry must reuse the exact complete job");
    assert_eq!(
        bound_job.as_deref(),
        accepted.body["jobId"].as_str(),
        "only verified complete evidence may bind the acceptance"
    );
    assert_eq!(active_job, bound_job);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn acceptance_reconciles_unbound_jobs_and_restart_replays_same_key() {
    let fixture = fixture("acceptance-unbound-terminal-retry");
    let app = application(&fixture);
    let bytes = source_package("legacy_id,name\nC-1,Synthetic\n");
    let expected_size = i64::try_from(bytes.len()).expect("bounded source size");
    let (preflight_id, ready) = create_and_preflight(
        &app,
        "Acceptance unbound terminal retry",
        "synthetic.zip",
        bytes,
    )
    .await;
    assert_eq!(ready["state"], "READY", "{ready:?}");
    let accept_body = json!({
        "schemaVersion": "migration-stream/engagement/write/accept-source/v1",
        "preflightId": preflight_id,
        "expectedSourceHash": ready["sourceHash"],
        "expectedManifestHash": ready["manifestHash"],
        "idempotencyKey": "acceptance-unbound-terminal-retry-0001",
        "acknowledgement": "migration-stream/source-acceptance/v1"
    });
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open database");
    connection
        .execute_batch(
            "CREATE TRIGGER reject_test_acceptance_job_create
             BEFORE INSERT ON jobs
             BEGIN
                 SELECT RAISE(ABORT, 'synthetic intake job creation failure');
             END;",
        )
        .expect("install deterministic post-attempt failure");
    drop(connection);
    let failed = command_json(
        &app,
        Method::POST,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/accept"),
        "engagement-source-accept-v1",
        "acceptance-unbound-terminal-retry-0001",
        accept_body.clone(),
    )
    .await;
    assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);

    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("seed crash boundary");
    connection
        .execute_batch("DROP TRIGGER reject_test_acceptance_job_create;")
        .expect("remove deterministic create fault");
    let controlled_relative_path: String = connection
        .query_row(
            "SELECT controlled_source_relative_path
             FROM engagement_acceptance_attempts
             WHERE idempotency_key = 'acceptance-unbound-terminal-retry-0001'",
            [],
            |row| row.get(0),
        )
        .expect("load controlled source identity");
    let controlled_source = fixture
        .workspace
        .canonicalize()
        .expect("canonical startup-selected workspace")
        .join(controlled_relative_path)
        .to_string_lossy()
        .into_owned();
    connection
        .execute(
            "INSERT INTO jobs (
                 id, state, source_name, source_path, source_size, policy_json,
                 error_code, error_message, created_at, updated_at, completed_at
             ) VALUES (
                 'failed-unbound-acceptance-job', 'FAILED', 'synthetic.zip', ?1, ?2, '{}',
                 'SYNTHETIC_FAILURE', 'synthetic unbound failure',
                 '2026-07-29T00:00:00.000Z', '2026-07-29T00:00:00.000Z',
                 '2026-07-29T00:00:00.000Z'
             )",
            rusqlite::params![controlled_source, expected_size],
        )
        .expect("seed failed unbound acceptance job");
    connection
        .execute(
            "INSERT INTO jobs (
                 id, state, source_name, source_path, source_size, policy_json,
                 created_at, updated_at
             ) VALUES (
                 'crashed-unbound-acceptance-job', 'SPOOLING', 'synthetic.zip', ?1, ?2, '{}',
                 '2026-07-29T00:00:01.000Z', '2026-07-29T00:00:01.000Z'
             )",
            rusqlite::params![controlled_source, expected_size],
        )
        .expect("seed unfinished acceptance job at the crash boundary");
    for (job_id, state, error_code) in [
        (
            "failed-unbound-acceptance-job",
            "FAILED",
            Some("SYNTHETIC_FAILURE"),
        ),
        ("crashed-unbound-acceptance-job", "SPOOLING", None),
    ] {
        connection
            .execute(
                "INSERT INTO job_events (
                     job_id, state, error_code, detail, occurred_at
                 ) VALUES (?1, ?2, ?3, 'synthetic acceptance retry fixture',
                           '2026-07-29T00:00:02.000Z')",
                rusqlite::params![job_id, state, error_code],
            )
            .expect("seed matching job event");
    }
    drop(connection);

    let accepted = command_json(
        &app,
        Method::POST,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/accept"),
        "engagement-source-accept-v1",
        "acceptance-unbound-terminal-retry-0001",
        accept_body.clone(),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{:?}", accepted.body);
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("inspect safe retry");
    let (failed_jobs, interrupted_jobs, complete_jobs, active_job): (
        i64,
        i64,
        i64,
        Option<String>,
    ) = connection
        .query_row(
            "SELECT
                 (SELECT COUNT(*) FROM jobs WHERE state = 'FAILED'),
                 (SELECT COUNT(*) FROM jobs WHERE state = 'INTERRUPTED'),
                 (SELECT COUNT(*) FROM jobs WHERE state = 'COMPLETE'),
                 (SELECT active_job_id FROM engagement_active_context)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("load terminal retry evidence");
    assert_eq!((failed_jobs, interrupted_jobs, complete_jobs), (1, 1, 1));
    assert_eq!(
        active_job.as_deref(),
        accepted.body["jobId"].as_str(),
        "only the newly completed exact job may become active"
    );
    let acceptance_recovery_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM job_events
             WHERE job_id = 'crashed-unbound-acceptance-job'
               AND state = 'INTERRUPTED'
               AND error_code = 'INTERRUPTED_ACCEPTANCE_RECOVERED'",
            [],
            |row| row.get(0),
        )
        .expect("count identity-bound acceptance recovery events");
    assert_eq!(acceptance_recovery_events, 1);
    let accepted_job = accepted.body["jobId"]
        .as_str()
        .expect("accepted job ID")
        .to_owned();
    drop(connection);
    drop(app);

    let reopened = application_without_establishing_workspace(&fixture);
    let replay = command_json(
        &reopened,
        Method::POST,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/accept"),
        "engagement-source-accept-v1",
        "acceptance-unbound-terminal-retry-0001",
        accept_body,
    )
    .await;
    assert_eq!(replay.status, StatusCode::OK, "{:?}", replay.body);
    assert_eq!(replay.body["jobId"], accepted_job);
    let jobs: i64 = Connection::open(fixture.workspace.join("state.sqlite3"))
        .expect("inspect restart replay")
        .query_row("SELECT COUNT(*) FROM jobs", [], |row| row.get(0))
        .expect("count restart jobs");
    assert_eq!(jobs, 3, "restart retry must not create another intake job");
}

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn acceptance_rejects_external_alias_and_dangling_destination_then_retries_safely() {
    use std::os::unix::fs::symlink;

    let fixture = fixture("acceptance-alias-boundary");
    let app = application(&fixture);
    let (preflight_id, ready) = create_and_preflight(
        &app,
        "Acceptance alias boundary",
        "synthetic.zip",
        source_package("legacy_id,name\nC-1,Synthetic\n"),
    )
    .await;
    assert_eq!(ready["state"], "READY", "{ready:?}");
    let source_hash = ready["sourceHash"].as_str().expect("source hash");
    let retained = fixture
        .workspace
        .join(format!("engagement/preflights/retained/{source_hash}.zip"));
    let external_alias = fixture
        .workspace
        .parent()
        .expect("fixture parent")
        .join("external-retained-alias.zip");
    fs::hard_link(&retained, &external_alias).expect("create external retained alias");
    let accept_body = json!({
        "schemaVersion": "migration-stream/engagement/write/accept-source/v1",
        "preflightId": preflight_id,
        "expectedSourceHash": ready["sourceHash"],
        "expectedManifestHash": ready["manifestHash"],
        "idempotencyKey": "acceptance-alias-0001",
        "acknowledgement": "migration-stream/source-acceptance/v1"
    });
    let alias_rejected = command_json(
        &app,
        Method::POST,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/accept"),
        "engagement-source-accept-v1",
        "acceptance-alias-0001",
        accept_body.clone(),
    )
    .await;
    assert_eq!(alias_rejected.status, StatusCode::CONFLICT);
    assert!(
        !alias_rejected
            .body
            .to_string()
            .contains(fixture.workspace.to_string_lossy().as_ref())
    );
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("open database");
    let attempt_id: String = connection
        .query_row(
            "SELECT attempt_id FROM engagement_acceptance_attempts
             WHERE idempotency_key = 'acceptance-alias-0001'",
            [],
            |row| row.get(0),
        )
        .expect("durable acceptance intent");
    drop(connection);
    fs::remove_file(&external_alias).expect("remove external alias");
    let acceptance_directory = fixture.workspace.join("engagement/acceptances");
    fs::create_dir_all(&acceptance_directory).expect("create acceptance directory");
    let destination = acceptance_directory.join(format!("{attempt_id}.zip"));
    let external_target = fixture
        .workspace
        .parent()
        .expect("fixture parent")
        .join("must-not-be-created.zip");
    symlink(&external_target, &destination).expect("install dangling destination symlink");
    let symlink_rejected = command_json(
        &app,
        Method::POST,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/accept"),
        "engagement-source-accept-v1",
        "acceptance-alias-0001",
        accept_body.clone(),
    )
    .await;
    assert_eq!(symlink_rejected.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!external_target.exists());
    assert!(
        fs::symlink_metadata(&destination)
            .expect("destination symlink remains")
            .file_type()
            .is_symlink()
    );

    fs::remove_file(&destination).expect("remove adversarial symlink");
    let accepted = command_json(
        &app,
        Method::POST,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/accept"),
        "engagement-source-accept-v1",
        "acceptance-alias-0001",
        accept_body,
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{:?}", accepted.body);
    assert!(destination.exists());
    let connection =
        Connection::open(fixture.workspace.join("state.sqlite3")).expect("reopen database");
    let attempts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_acceptance_attempts
             WHERE idempotency_key = 'acceptance-alias-0001'",
            [],
            |row| row.get(0),
        )
        .expect("count attempts");
    assert_eq!(attempts, 1);
}

#[tokio::test]
async fn invalid_zip_fails_intelligibly_and_cannot_be_accepted() {
    let fixture = fixture("fresh-invalid");
    let app = application(&fixture);
    let (preflight_id, response) = create_and_preflight(
        &app,
        "Invalid sanitized package proof",
        "not-a-zip.zip",
        b"this is not a ZIP".to_vec(),
    )
    .await;
    assert_eq!(
        response["schema_version"], "migration-stream/http-error/v1",
        "{response:?}"
    );
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "ARCHIVE_INTEGRITY");
    assert!(
        response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("safety preflight"))
    );

    let detail = get_json(&app, "/api/v1/engagement").await;
    assert_eq!(detail.status, StatusCode::OK, "{:?}", detail.body);
    let rejected = detail.body["preflights"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["preflightId"] == preflight_id)
        })
        .expect("rejected preflight remains visible");
    assert_eq!(rejected["state"], "REJECTED");
    assert_eq!(rejected["errorCode"], "ARCHIVE_INTEGRITY");

    let fake_hash = migration_stream::canonical::lower_hex(Sha256::digest(b"this is not a ZIP"));
    let accept = command_json(
        &app,
        Method::POST,
        &format!("/api/v1/engagement/source-preflights/{preflight_id}/accept"),
        "engagement-source-accept-v1",
        "test-invalid-accept01",
        json!({
            "schemaVersion": "migration-stream/engagement/write/accept-source/v1",
            "preflightId": preflight_id,
            "expectedSourceHash": fake_hash,
            "expectedManifestHash": "0".repeat(64),
            "idempotencyKey": "test-invalid-accept01",
            "acknowledgement": "migration-stream/source-acceptance/v1"
        }),
    )
    .await;
    assert_eq!(accept.status, StatusCode::CONFLICT, "{:?}", accept.body);
    assert_eq!(accept.body["ok"], false);
}

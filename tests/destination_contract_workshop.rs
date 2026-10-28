use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use migration_stream::{
    KernelError,
    canonical::{canonical_hash, canonical_json_string},
    destination::{
        APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA, ApproveDestinationProfileCommand,
        ApprovedDestinationProfileView, CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA,
        CompatibilityAcknowledgement, CreateManualCatalogueCommand,
        DESTINATION_FREEZE_ACKNOWLEDGEMENT, DestinationArtifactView, DestinationDiffState,
        DestinationDraftSnapshot, DestinationDraftView, DestinationFieldDraft,
        DestinationFieldState, DestinationRelationshipDraft, DestinationRelationshipKeyPair,
        DestinationRuleDraft, FREEZE_DRAFT_COMMAND_SCHEMA, FreezeDestinationDraftCommand,
        FrozenDestinationProfileView, LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT,
        ManualEntityDefinition, ManualPropertyDefinition, ManualSupplementEvidence,
        REBASE_DRAFT_COMMAND_SCHEMA, RESERVE_EDMX_COMMAND_SCHEMA, RebaseDestinationDraftCommand,
        ReserveEdmxCommand, SAVE_DRAFT_COMMAND_SCHEMA, START_DRAFT_COMMAND_SCHEMA,
        SaveDestinationDraftCommand, StartDestinationDraftCommand, TechnicalChangeDecision,
        TechnicalChangeReviewEvidence,
    },
    engagement::{
        CREATE_ENGAGEMENT_COMMAND_SCHEMA, ControlledObjectPack, CreateOrReopenCommand,
        EngagementService, EngagementServiceConfig, MappingMode,
        SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID,
    },
    engagement_workbench::{EngagementWorkbenchConfig, build_engagement_application_router},
    journey::{JourneyStageId, JourneyStageStatus},
    object_workspace::{ObjectWorkspaceRecord, establish_for_local_automation},
    policy::IntakePolicy,
    profiling::ProfilingPolicy,
    state::integrity_check,
    validation::ValidationPolicy,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt as _;

const ROOT_TECHNICAL_ENTITY: &str = "Synthetic.VendorExtended.Sales.SalesOrder";
const CHILD_TECHNICAL_ENTITY: &str = "Synthetic.VendorExtended.Sales.SalesOrderItem";
const ROOT_ENTITY: &str = "salesOrder";
const CHILD_ENTITY: &str = "salesOrderItem";
const PROFILE_ID: &str = "synthetic.workspace.sales-document";

fn repository_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn controlled_pack() -> ControlledObjectPack {
    ControlledObjectPack {
        object_pack_id: SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID.to_owned(),
        version: "1.0.0".to_owned(),
        title: "Synthetic sales-document compatibility pack".to_owned(),
        description: "Existing controlled pack retained only for compatibility.".to_owned(),
        scope: "Synthetic target-neutral integration proof.".to_owned(),
        root_entity_id: "SalesDocument".to_owned(),
        mapping_mode: MappingMode::OperatorGoverned,
        limitations: vec!["No target-system compatibility claim.".to_owned()],
        mapping_suggestions: Vec::new(),
        reference_tables: Vec::new(),
        profile_document: serde_json::from_slice(
            &fs::read(repository_path(
                "object-packs/synthetic.sales-document.readiness/1.0.0/synthetic-profile.reviewed.json",
            ))
            .expect("read compatibility profile"),
        )
        .expect("parse compatibility profile"),
    }
}

fn open_service(workspace: &Path) -> EngagementService {
    EngagementService::new(EngagementServiceConfig {
        workspace: workspace.to_path_buf(),
        intake_policy: IntakePolicy::from_path(Some(&repository_path(
            "config/engagement-intake-policy.json",
        )))
        .expect("intake policy"),
        profiling_policy: ProfilingPolicy::from_path(&repository_path(
            "config/engagement-profiling-policy.json",
        ))
        .expect("profiling policy"),
        profile_schema_path: repository_path("schemas/migration-object-profile.schema.json"),
        mapping_schema_path: repository_path("schemas/source-mapping-version.schema.json"),
        validation_policy: ValidationPolicy::from_path(&repository_path(
            "config/engagement-validation-policy.json",
        ))
        .expect("validation policy"),
        object_packs: vec![controlled_pack()],
    })
    .expect("open engagement service")
}

fn create_workspace_and_engagement(workspace: &Path) -> (EngagementService, ObjectWorkspaceRecord) {
    let record = establish_for_local_automation(
        workspace,
        "Destination contract proof",
        "Synthetic sales document",
        "Destination workshop integration test",
    )
    .expect("establish object workspace");
    let service = open_service(workspace);
    let engagement = service
        .create_or_reopen(&CreateOrReopenCommand {
            schema_version: CREATE_ENGAGEMENT_COMMAND_SCHEMA.to_owned(),
            display_name: "Destination contract workshop proof".to_owned(),
            idempotency_key: "destination-create-engagement-0001".to_owned(),
        })
        .expect("create engagement");
    assert!(!engagement.reopened);
    (service, record)
}

fn reopen_engagement(workspace: &Path) -> EngagementService {
    let service = open_service(workspace);
    let engagement = service
        .create_or_reopen(&CreateOrReopenCommand {
            schema_version: CREATE_ENGAGEMENT_COMMAND_SCHEMA.to_owned(),
            display_name: "Destination contract workshop proof".to_owned(),
            idempotency_key: "destination-reopen-engagement-0001".to_owned(),
        })
        .expect("reopen engagement");
    assert!(engagement.reopened);
    service
}

fn import_edmx(
    service: &EngagementService,
    workspace: &ObjectWorkspaceRecord,
    fixture: &str,
    suffix: &str,
) -> DestinationArtifactView {
    import_destination_bytes(
        service,
        workspace,
        &fs::read(repository_path(fixture)).expect("read EDMX fixture"),
        suffix,
    )
}

fn import_destination_bytes(
    service: &EngagementService,
    workspace: &ObjectWorkspaceRecord,
    bytes: &[u8],
    suffix: &str,
) -> DestinationArtifactView {
    let generation = service.state().expect("engagement state").generation;
    let destination = service.destination_service();
    let idempotency_key = format!("destination-edmx-reserve-{suffix}-0001");
    let reservation = destination
        .reserve_edmx(&ReserveEdmxCommand {
            schema_version: RESERVE_EDMX_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: generation,
            expected_workspace_id: workspace.workspace_id.clone(),
            expected_workspace_descriptor_hash: workspace.descriptor_hash.clone(),
            source_name: format!("{suffix}.edmx"),
            artifact_label: format!("Synthetic sales destination {suffix}"),
            system: Some("synthetic-offline".to_owned()),
            client_or_tenant: Some("test-tenant".to_owned()),
            release: Some("fixture-v1".to_owned()),
            idempotency_key: idempotency_key.clone(),
        })
        .expect("reserve destination EDMX");
    assert_eq!(reservation.state, "RECEIVING");
    let mut upload = destination
        .open_edmx_upload(&reservation.attempt_id, &idempotency_key)
        .expect("open controlled upload");
    upload.write_all(bytes).expect("write EDMX upload");
    destination
        .complete_edmx_upload(&reservation.attempt_id, &idempotency_key, upload)
        .expect("complete destination EDMX")
}

fn required_rule(entity_id: &str, field_id: &str, ordinal: usize) -> DestinationRuleDraft {
    DestinationRuleDraft {
        rule_id: format!("required-{ordinal:02}"),
        severity: "error".to_owned(),
        kind: "required".to_owned(),
        target: format!("{entity_id}.{field_id}"),
        message: format!("{field_id} is required by the reviewed destination contract."),
        parameters: json!({}),
    }
}

fn reviewed_snapshot(initial: &DestinationDraftSnapshot) -> DestinationDraftSnapshot {
    let mut snapshot = initial.clone();
    let mut rules = Vec::new();
    let mut ordinal = 0_usize;
    for entity in &mut snapshot.entities {
        for field in &mut entity.fields {
            field.state = DestinationFieldState::Active;
            field.business_required = true;
            ordinal = ordinal.saturating_add(1);
            rules.push(required_rule(&entity.entity_id, &field.field_id, ordinal));
        }
    }
    let root = snapshot
        .entities
        .iter_mut()
        .find(|entity| entity.root)
        .expect("root entity");
    root.fields.push(DestinationFieldDraft {
        field_id: "purchaseOrderByCustomer".to_owned(),
        label: "Purchase order by customer".to_owned(),
        logical_type: "text".to_owned(),
        business_required: false,
        sensitive: false,
        key: false,
        state: DestinationFieldState::Active,
        technical_property: Some(format!("{ROOT_TECHNICAL_ENTITY}.PurchaseOrderByCustomer")),
        manual_supplement: None,
        technical_change_review: None,
        confirm_max_length: false,
        max_length: Some(35),
        diff_state: None,
    });
    root.fields.push(DestinationFieldDraft {
        field_id: "extensionReference".to_owned(),
        label: "extensionReference".to_owned(),
        logical_type: "text".to_owned(),
        business_required: false,
        sensitive: false,
        key: false,
        state: DestinationFieldState::Active,
        technical_property: None,
        manual_supplement: Some(ManualSupplementEvidence {
            supplement_id: "manual-extension-reference".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Required by the reviewed migration scope but absent from the first EDMX."
                .to_owned(),
        }),
        technical_change_review: None,
        confirm_max_length: false,
        max_length: None,
        diff_state: None,
    });
    root.fields
        .sort_by(|left, right| left.field_id.cmp(&right.field_id));
    snapshot.relationships = vec![DestinationRelationshipDraft {
        relationship_id: "sales-order-items".to_owned(),
        from_entity_id: ROOT_ENTITY.to_owned(),
        to_entity_id: CHILD_ENTITY.to_owned(),
        cardinality: "1..*".to_owned(),
        technical_navigation: Some(format!("{ROOT_TECHNICAL_ENTITY}.to_Item")),
        key_pairs: vec![DestinationRelationshipKeyPair {
            from_field_id: "salesOrder".to_owned(),
            to_field_id: "salesOrder".to_owned(),
        }],
        confirmed: true,
    }];
    snapshot.rules = rules;
    snapshot
}

fn confirm_all_suggested_fields(initial: &DestinationDraftSnapshot) -> DestinationDraftSnapshot {
    let mut snapshot = initial.clone();
    let mut ordinal = 0_usize;
    let mut rules = Vec::new();
    for entity in &mut snapshot.entities {
        for field in &mut entity.fields {
            field.state = DestinationFieldState::Active;
            field.business_required = true;
            ordinal = ordinal.saturating_add(1);
            rules.push(required_rule(&entity.entity_id, &field.field_id, ordinal));
        }
    }
    snapshot.rules = rules;
    snapshot
}

fn reviewed_relationship(confirmed: bool) -> DestinationRelationshipDraft {
    DestinationRelationshipDraft {
        relationship_id: "sales-order-items".to_owned(),
        from_entity_id: ROOT_ENTITY.to_owned(),
        to_entity_id: CHILD_ENTITY.to_owned(),
        cardinality: "1..*".to_owned(),
        technical_navigation: Some(format!("{ROOT_TECHNICAL_ENTITY}.to_Item")),
        key_pairs: vec![DestinationRelationshipKeyPair {
            from_field_id: "salesOrder".to_owned(),
            to_field_id: "salesOrder".to_owned(),
        }],
        confirmed,
    }
}

fn start_single_entity_draft(
    service: &EngagementService,
    artifact: &DestinationArtifactView,
    technical_entity: &str,
    profile_id: &str,
    suffix: &str,
) -> DestinationDraftView {
    service
        .destination_service()
        .start_draft(&StartDestinationDraftCommand {
            schema_version: START_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            catalogue_authority_id: artifact.catalogue_authority_id.clone(),
            expected_catalogue_hash: artifact.catalogue_hash.clone(),
            root_technical_entity: technical_entity.to_owned(),
            child_technical_entities: Vec::new(),
            profile_id: profile_id.to_owned(),
            title: format!("Synthetic destination scope {suffix}"),
            owner: "Destination practitioner".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Select one exact bounded technical entity.".to_owned(),
            idempotency_key: format!("destination-start-{suffix}-draft-0001"),
        })
        .expect("start single-entity destination draft")
}

fn create_approved_original(
    service: &EngagementService,
    workspace: &ObjectWorkspaceRecord,
    suffix: &str,
) -> (
    DestinationArtifactView,
    DestinationDraftView,
    FrozenDestinationProfileView,
    ApprovedDestinationProfileView,
) {
    let artifact = import_edmx(
        service,
        workspace,
        "fixtures/metadata/sanitized-sales-v2.edmx",
        suffix,
    );
    let draft = service
        .destination_service()
        .start_draft(&StartDestinationDraftCommand {
            schema_version: START_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            catalogue_authority_id: artifact.catalogue_authority_id.clone(),
            expected_catalogue_hash: artifact.catalogue_hash.clone(),
            root_technical_entity: ROOT_TECHNICAL_ENTITY.to_owned(),
            child_technical_entities: vec![CHILD_TECHNICAL_ENTITY.to_owned()],
            profile_id: PROFILE_ID.to_owned(),
            title: "Synthetic workspace sales document".to_owned(),
            owner: "Destination practitioner".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Define the bounded destination scope.".to_owned(),
            idempotency_key: format!("destination-start-{suffix}-draft-0001"),
        })
        .expect("start original destination draft");
    let saved = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id.clone(),
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash.clone(),
            snapshot: reviewed_snapshot(&draft.snapshot),
            actor: "Destination practitioner".to_owned(),
            reason: "Confirm the initial destination contract.".to_owned(),
            idempotency_key: format!("destination-save-{suffix}-draft-0001"),
        })
        .expect("save original destination draft");
    let frozen = service
        .destination_service()
        .freeze_draft(&FreezeDestinationDraftCommand {
            schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: saved.draft_id.clone(),
            expected_draft_generation: saved.generation,
            expected_draft_hash: saved.draft_hash.clone(),
            profile_version: "1.0.0".to_owned(),
            acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
            actor: "Destination reviewer".to_owned(),
            reason: "Freeze the original destination contract.".to_owned(),
            idempotency_key: format!("destination-freeze-{suffix}-profile-0001"),
        })
        .expect("freeze original destination profile");
    let approved = service
        .destination_service()
        .approve_destination_profile(&ApproveDestinationProfileCommand {
            schema_version: APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            profile_id: frozen.profile.profile_id.clone(),
            profile_version: frozen.profile.profile_version.clone(),
            profile_hash: frozen.profile.profile_hash.clone(),
            origin_binding_hash: frozen.origin_binding_hash.clone(),
            actor: "Destination approver".to_owned(),
            reason: "Approve only local mapping and validation use.".to_owned(),
            authority_acknowledgement: LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT.to_owned(),
            idempotency_key: format!("destination-approve-{suffix}-profile-0001"),
        })
        .expect("approve original destination profile");
    (artifact, saved, frozen, approved)
}

fn assert_initial_technical_suggestions(snapshot: &DestinationDraftSnapshot) {
    assert_eq!(snapshot.entities.len(), 2);
    let root = snapshot
        .entities
        .iter()
        .find(|entity| entity.root)
        .expect("root suggestion");
    let child = snapshot
        .entities
        .iter()
        .find(|entity| !entity.root)
        .expect("child suggestion");
    assert_eq!(root.fields.len(), 5);
    assert_eq!(child.fields.len(), 5);
    assert!(
        snapshot
            .entities
            .iter()
            .flat_map(|entity| &entity.fields)
            .all(|field| {
                field.state == DestinationFieldState::NeedsReview
                    && field.technical_property.is_some()
                    && !field.business_required
            })
    );
    assert!(
        root.fields
            .iter()
            .any(|field| field.field_id == "salesOrder" && field.key)
    );
    assert_eq!(
        child
            .fields
            .iter()
            .filter(|field| field.key)
            .map(|field| field.field_id.as_str())
            .collect::<Vec<_>>(),
        vec!["salesOrder", "salesOrderItem"]
    );
}

fn stage(
    journey: &migration_stream::journey::JourneyView,
    id: JourneyStageId,
) -> &migration_stream::journey::JourneyStage {
    journey
        .stages
        .iter()
        .find(|stage| stage.id == id)
        .expect("journey stage")
}

struct HttpJsonResponse {
    status: StatusCode,
    body: Value,
}

fn destination_http_application(workspace: &Path, support_root: &Path) -> Router {
    let assets = support_root.join("assets");
    let object_packs = support_root.join("object-packs");
    let pack_version = object_packs
        .join(SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID)
        .join("1.0.0");
    fs::create_dir_all(&assets).expect("create destination HTTP asset directory");
    fs::create_dir_all(&pack_version).expect("create destination HTTP pack catalogue");
    for file_name in ["manifest.json", "synthetic-profile.reviewed.json"] {
        fs::copy(
            repository_path(&format!(
                "object-packs/{SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID}/1.0.0/{file_name}"
            )),
            pack_version.join(file_name),
        )
        .expect("copy destination HTTP controlled pack");
    }
    fs::write(
        assets.join("index.html"),
        "<!doctype html><title>Destination contract HTTP regression</title>",
    )
    .expect("write destination HTTP test asset");
    build_engagement_application_router(
        EngagementWorkbenchConfig {
            workspace: workspace.to_path_buf(),
            intake_policy: repository_path("config/engagement-intake-policy.json"),
            profiling_policy: repository_path("config/engagement-profiling-policy.json"),
            validation_policy: repository_path("config/engagement-validation-policy.json"),
            object_packs,
            profile_schema: repository_path("schemas/migration-object-profile.schema.json"),
            mapping_schema: repository_path("schemas/source-mapping-version.schema.json"),
        },
        assets,
    )
    .expect("build destination HTTP application")
}

async fn destination_http_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Body,
    command: Option<(&str, &str)>,
) -> HttpJsonResponse {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:4198");
    if let Some((intent, idempotency_key)) = command {
        builder = builder
            .header("origin", "http://127.0.0.1:4198")
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header("x-migration-stream-intent", intent)
            .header("idempotency-key", idempotency_key);
    }
    let response = app
        .clone()
        .oneshot(builder.body(body).expect("build destination HTTP request"))
        .await
        .expect("infallible destination HTTP router");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("read bounded destination HTTP response");
    let body = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "destination HTTP response must be JSON ({error}); bytes={:?}",
            String::from_utf8_lossy(&bytes)
        )
    });
    HttpJsonResponse { status, body }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn http_freeze_remains_reviewed_until_separate_exact_hash_approval() {
    let temporary = TempDir::new().expect("temporary destination HTTP workspace");
    let workspace_path = temporary.path().join("workspace");
    let support_root = temporary.path().join("http-support");
    fs::create_dir(&workspace_path).expect("create destination HTTP workspace directory");
    let (service, workspace) = create_workspace_and_engagement(&workspace_path);
    let artifact = import_edmx(
        &service,
        &workspace,
        "fixtures/metadata/sanitized-sales-v2.edmx",
        "http-freeze",
    );
    let draft = service
        .destination_service()
        .start_draft(&StartDestinationDraftCommand {
            schema_version: START_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            catalogue_authority_id: artifact.catalogue_authority_id,
            expected_catalogue_hash: artifact.catalogue_hash,
            root_technical_entity: ROOT_TECHNICAL_ENTITY.to_owned(),
            child_technical_entities: vec![CHILD_TECHNICAL_ENTITY.to_owned()],
            profile_id: "workspace.sales-document.http-freeze".to_owned(),
            title: "Workspace sales document HTTP freeze".to_owned(),
            owner: "Destination practitioner".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Select the exact destination scope for the HTTP regression.".to_owned(),
            idempotency_key: "destination-http-start-draft-0001".to_owned(),
        })
        .expect("start HTTP destination draft");
    let saved = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id,
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash,
            snapshot: reviewed_snapshot(&draft.snapshot),
            actor: "Destination practitioner".to_owned(),
            reason: "Confirm the exact HTTP destination draft.".to_owned(),
            idempotency_key: "destination-http-save-draft-0001".to_owned(),
        })
        .expect("save HTTP destination draft");
    let freeze_command = FreezeDestinationDraftCommand {
        schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
        expected_engagement_generation: service.state().expect("state").generation,
        draft_id: saved.draft_id,
        expected_draft_generation: saved.generation,
        expected_draft_hash: saved.draft_hash,
        profile_version: "1.0.0".to_owned(),
        acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
        actor: "Destination reviewer".to_owned(),
        reason: "Freeze reviewed bytes without granting mapping authority.".to_owned(),
        idempotency_key: "destination-http-freeze-profile-0001".to_owned(),
    };
    drop(service);
    let app = destination_http_application(&workspace_path, &support_root);

    let frozen = destination_http_json(
        &app,
        Method::POST,
        "/api/v1/engagement/destination/drafts/freeze",
        Body::from(serde_json::to_vec(&freeze_command).expect("serialize freeze command")),
        Some((
            "engagement-destination-draft-freeze-v1",
            &freeze_command.idempotency_key,
        )),
    )
    .await;
    assert_eq!(frozen.status, StatusCode::OK, "{:?}", frozen.body);
    assert_eq!(frozen.body["profile"]["declared_status"], "REVIEWED_DRAFT");
    assert_eq!(frozen.body["profile"]["lifecycle_status"], "REVIEWED_DRAFT");
    assert_eq!(frozen.body["profile"]["authority"], "NONE");
    assert!(frozen.body["profile"]["approval"].is_null());
    let profile_hash = frozen.body["profile"]["profile_hash"]
        .as_str()
        .expect("frozen profile hash")
        .to_owned();

    let context =
        destination_http_json(&app, Method::GET, "/api/v1/context", Body::empty(), None).await;
    assert_eq!(context.status, StatusCode::OK, "{:?}", context.body);
    assert_eq!(context.body["engagement"]["profileStage"], "REVIEWED");
    assert_eq!(
        context.body["engagement"]["active"]["profileHash"],
        profile_hash
    );

    let journey =
        destination_http_json(&app, Method::GET, "/api/v1/journey", Body::empty(), None).await;
    assert_eq!(journey.status, StatusCode::OK, "{:?}", journey.body);
    assert_eq!(journey.body["currentStageId"], "DESTINATION");
    assert_eq!(journey.body["binding"]["profileHash"], profile_hash);
    let destination_stage = journey.body["stages"]
        .as_array()
        .expect("journey stages")
        .iter()
        .find(|stage| stage["id"] == "DESTINATION")
        .expect("destination stage");
    assert_eq!(destination_stage["status"], "CURRENT");
    let mapping_stage = journey.body["stages"]
        .as_array()
        .expect("journey stages")
        .iter()
        .find(|stage| stage["id"] == "MAPPING")
        .expect("mapping stage");
    assert_eq!(mapping_stage["status"], "LOCKED");

    let destination = destination_http_json(
        &app,
        Method::GET,
        "/api/v1/engagement/destination",
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(destination.status, StatusCode::OK, "{:?}", destination.body);
    assert_eq!(
        destination.body["activeDestination"]["profileHash"],
        profile_hash
    );
    assert_eq!(
        destination.body["activeDestination"]["profileAuthority"],
        "NONE"
    );
    assert!(destination.body["activeDestination"]["approvalId"].is_null());
    assert!(destination.body["activeDestination"]["approvalHash"].is_null());
    assert_eq!(
        destination.body["reviewedCandidate"]["profile"]["profile_hash"],
        profile_hash
    );

    let mapping = destination_http_json(
        &app,
        Method::GET,
        "/api/v1/engagement/mapping-workspace",
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(mapping.status, StatusCode::CONFLICT, "{:?}", mapping.body);
    assert_eq!(mapping.body["error"]["code"], "PROFILE_NOT_APPROVED");
    drop(app);

    let connection =
        Connection::open(workspace_path.join("state.sqlite3")).expect("inspect HTTP freeze state");
    let durable: (String, i64, i64, i64) = connection
        .query_row(
            "SELECT context.profile_stage,
                    (SELECT COUNT(*) FROM profile_approvals),
                    (SELECT COUNT(*) FROM engagement_mapping_draft_heads),
                    (SELECT COUNT(*) FROM mapping_versions)
             FROM engagement_active_context AS context",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read durable HTTP freeze authority");
    assert_eq!(durable, ("REVIEWED".to_owned(), 0, 0, 0));
    drop(connection);
    let integrity = integrity_check(&workspace_path).expect("HTTP freeze integrity replay");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn destination_authority_survives_reopen_and_rebases_a_frozen_profile() {
    let temporary = TempDir::new().expect("temporary workspace");
    let (service, workspace) = create_workspace_and_engagement(temporary.path());

    let initial_journey = service.journey().expect("initial journey");
    assert_eq!(
        initial_journey.current_stage_id,
        JourneyStageId::Destination
    );
    assert_eq!(
        stage(&initial_journey, JourneyStageId::Source).status,
        JourneyStageStatus::Available
    );

    let original = import_edmx(
        &service,
        &workspace,
        "fixtures/metadata/sanitized-sales-v2.edmx",
        "original",
    );
    assert_eq!(original.origin, "EDMX");
    assert_eq!(original.odata_generation.as_deref(), Some("V2"));
    assert_eq!(original.system.as_deref(), Some("synthetic-offline"));
    assert_eq!(original.client_or_tenant.as_deref(), Some("test-tenant"));
    assert_eq!(original.release.as_deref(), Some("fixture-v1"));
    assert!(original.manual_actor.is_none());
    assert!(original.manual_reason.is_none());
    assert_eq!(original.entity_count, 2);
    assert!(original.property_count > 20);
    assert!(original.compatibility.unsupported_inventory_count > 0);
    assert_eq!(original.compatibility.unsupported_irrelevant_count, 0);
    assert!(original.compatibility.scoped_completeness_hash.is_none());

    let entities_first = service
        .destination_service()
        .catalogue_entities(&original.catalogue_authority_id, None, 1, None)
        .expect("first entity page");
    assert_eq!(entities_first.items.len(), 1);
    let entities_second = service
        .destination_service()
        .catalogue_entities(
            &original.catalogue_authority_id,
            None,
            1,
            entities_first.next_cursor.as_deref(),
        )
        .expect("second entity page");
    assert_eq!(entities_second.items.len(), 1);
    assert_ne!(
        entities_first.items[0].technical_entity,
        entities_second.items[0].technical_entity
    );

    let pre_draft_properties = service
        .destination_service()
        .catalogue_properties(
            &original.catalogue_authority_id,
            ROOT_TECHNICAL_ENTITY,
            None,
            None,
            2,
            None,
        )
        .expect("first property page");
    assert_eq!(pre_draft_properties.items.len(), 2);
    assert!(pre_draft_properties.next_cursor.is_some());
    assert!(
        pre_draft_properties
            .items
            .iter()
            .all(|property| property.state == DestinationFieldState::Available)
    );

    let wrong_catalogue = service
        .destination_service()
        .start_draft(&StartDestinationDraftCommand {
            schema_version: START_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            catalogue_authority_id: original.catalogue_authority_id.clone(),
            expected_catalogue_hash: "0".repeat(64),
            root_technical_entity: ROOT_TECHNICAL_ENTITY.to_owned(),
            child_technical_entities: vec![CHILD_TECHNICAL_ENTITY.to_owned()],
            profile_id: PROFILE_ID.to_owned(),
            title: "Synthetic workspace sales document".to_owned(),
            owner: "Destination practitioner".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Define the bounded destination scope.".to_owned(),
            idempotency_key: "destination-start-wrong-catalogue-0001".to_owned(),
        })
        .expect_err("catalogue hash mismatch must fail closed");
    assert!(matches!(wrong_catalogue, KernelError::Conflict(_)));

    let start_command = StartDestinationDraftCommand {
        schema_version: START_DRAFT_COMMAND_SCHEMA.to_owned(),
        expected_engagement_generation: service.state().expect("state").generation,
        catalogue_authority_id: original.catalogue_authority_id.clone(),
        expected_catalogue_hash: original.catalogue_hash.clone(),
        root_technical_entity: ROOT_TECHNICAL_ENTITY.to_owned(),
        child_technical_entities: vec![CHILD_TECHNICAL_ENTITY.to_owned()],
        profile_id: PROFILE_ID.to_owned(),
        title: "Synthetic workspace sales document".to_owned(),
        owner: "Destination practitioner".to_owned(),
        actor: "Destination practitioner".to_owned(),
        reason: "Define the bounded destination scope.".to_owned(),
        idempotency_key: "destination-start-draft-0001".to_owned(),
    };
    let draft = service
        .destination_service()
        .start_draft(&start_command)
        .expect("start destination draft");
    assert_initial_technical_suggestions(&draft.snapshot);
    assert_ne!(draft.freeze_blockers, [] as [std::string::String; 0]);

    let root_properties = service
        .destination_service()
        .catalogue_properties(
            &original.catalogue_authority_id,
            ROOT_TECHNICAL_ENTITY,
            None,
            Some(DestinationFieldState::NeedsReview),
            100,
            None,
        )
        .expect("suggested root properties");
    assert_eq!(root_properties.items.len(), 5);
    assert!(
        root_properties
            .items
            .iter()
            .all(|property| property.key_candidate || property.required_candidate)
    );

    let save_command = SaveDestinationDraftCommand {
        schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
        expected_engagement_generation: service.state().expect("state").generation,
        draft_id: draft.draft_id.clone(),
        expected_draft_generation: draft.generation,
        expected_draft_hash: draft.draft_hash.clone(),
        snapshot: reviewed_snapshot(&draft.snapshot),
        actor: "Destination practitioner".to_owned(),
        reason: "Confirm technical suggestions and reviewed business meaning.".to_owned(),
        idempotency_key: "destination-save-reviewed-draft-0001".to_owned(),
    };
    let saved = service
        .destination_service()
        .save_draft(&save_command)
        .expect("save reviewed destination draft");
    assert_eq!(saved.generation, 2);
    assert_eq!(saved.freeze_blockers, [] as [std::string::String; 0]);
    assert_eq!(
        service
            .destination_service()
            .save_draft(&save_command)
            .expect("exact save retry"),
        saved
    );
    let mut changed_save_retry = save_command.clone();
    changed_save_retry.reason = "Same key, different command bytes must fail.".to_owned();
    let changed_save_retry = service
        .destination_service()
        .save_draft(&changed_save_retry)
        .expect_err("changed save retry must conflict");
    assert!(matches!(changed_save_retry, KernelError::Conflict(_)));

    let stale = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id.clone(),
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash.clone(),
            snapshot: reviewed_snapshot(&draft.snapshot),
            actor: "Destination practitioner".to_owned(),
            reason: "A stale editor must not replace the saved revision.".to_owned(),
            idempotency_key: "destination-save-stale-draft-0001".to_owned(),
        })
        .expect_err("stale draft CAS must fail");
    assert!(matches!(stale, KernelError::Conflict(_)));

    let before_reopen = service
        .destination_service()
        .workshop()
        .expect("workshop before reopen");
    drop(service);
    let service = reopen_engagement(temporary.path());
    let after_reopen = service
        .destination_service()
        .workshop()
        .expect("workshop after reopen");
    assert_eq!(after_reopen.workspace_id, before_reopen.workspace_id);
    assert_eq!(after_reopen.latest_artifact, before_reopen.latest_artifact);
    let reopened_artifact = after_reopen
        .latest_artifact
        .as_ref()
        .expect("reopened exact READY artifact");
    assert_eq!(
        reopened_artifact.system.as_deref(),
        Some("synthetic-offline")
    );
    assert_eq!(
        reopened_artifact.client_or_tenant.as_deref(),
        Some("test-tenant")
    );
    assert_eq!(reopened_artifact.release.as_deref(), Some("fixture-v1"));
    assert_eq!(after_reopen.draft, before_reopen.draft);

    let freeze_command = FreezeDestinationDraftCommand {
        schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
        expected_engagement_generation: service.state().expect("state").generation,
        draft_id: saved.draft_id.clone(),
        expected_draft_generation: saved.generation,
        expected_draft_hash: saved.draft_hash.clone(),
        profile_version: "1.0.0".to_owned(),
        acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
        actor: "Destination reviewer".to_owned(),
        reason: "Freeze the exact reviewed draft separately from approval.".to_owned(),
        idempotency_key: "destination-freeze-profile-0001".to_owned(),
    };
    let frozen = service
        .destination_service()
        .freeze_draft(&freeze_command)
        .expect("freeze reviewed destination profile");
    assert_eq!(frozen.profile.declared_status, "REVIEWED_DRAFT");
    assert!(frozen.profile.approval.is_none());
    assert_eq!(
        frozen.engagement_generation,
        service.state().expect("post-freeze state").generation
    );
    let reviewed_workshop = service
        .destination_service()
        .workshop()
        .expect("destination read after freeze and before approval");
    let reviewed_active = reviewed_workshop
        .active_destination
        .as_ref()
        .expect("reviewed destination identity after freeze");
    assert_eq!(reviewed_active.profile_hash, frozen.profile.profile_hash);
    assert_eq!(
        reviewed_active.profile_authority,
        migration_stream::profile_registry::ProfileAuthority::None
    );
    assert!(reviewed_active.approval_id.is_none());
    assert!(reviewed_active.approval_hash.is_none());
    let candidate_before_approval = reviewed_workshop
        .reviewed_candidate
        .expect("reviewed candidate after freeze");
    assert_eq!(candidate_before_approval, frozen);
    let reviewed_journey = service
        .journey()
        .expect("journey after freeze and before separate approval");
    assert_eq!(
        reviewed_journey.current_stage_id,
        JourneyStageId::Destination
    );
    assert_eq!(
        stage(&reviewed_journey, JourneyStageId::Destination).status,
        JourneyStageStatus::Current
    );
    assert_eq!(
        stage(&reviewed_journey, JourneyStageId::Source).status,
        JourneyStageStatus::Available
    );
    assert_eq!(
        stage(&reviewed_journey, JourneyStageId::Mapping).status,
        JourneyStageStatus::Locked
    );
    assert_eq!(
        reviewed_journey.binding.profile_hash.as_deref(),
        Some(frozen.profile.profile_hash.as_str())
    );
    drop(service);
    let service = reopen_engagement(temporary.path());
    assert_eq!(
        service
            .destination_service()
            .workshop()
            .expect("reopen frozen candidate")
            .reviewed_candidate,
        Some(frozen.clone())
    );
    assert_eq!(
        service
            .journey()
            .expect("reopen journey before separate destination approval")
            .current_stage_id,
        JourneyStageId::Destination
    );
    assert_eq!(
        service
            .destination_service()
            .freeze_draft(&freeze_command)
            .expect("exact freeze retry"),
        frozen
    );
    let mut changed_freeze_retry = freeze_command.clone();
    changed_freeze_retry.reason = "Same freeze key with different bytes must fail.".to_owned();
    let changed_freeze_retry = service
        .destination_service()
        .freeze_draft(&changed_freeze_retry)
        .expect_err("changed freeze retry must conflict");
    assert!(matches!(changed_freeze_retry, KernelError::Conflict(_)));

    let wrong_origin = service
        .destination_service()
        .approve_destination_profile(&ApproveDestinationProfileCommand {
            schema_version: APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            profile_id: frozen.profile.profile_id.clone(),
            profile_version: frozen.profile.profile_version.clone(),
            profile_hash: frozen.profile.profile_hash.clone(),
            origin_binding_hash: "0".repeat(64),
            actor: "Destination approver".to_owned(),
            reason: "This deliberately mismatched origin must not approve.".to_owned(),
            authority_acknowledgement: LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT.to_owned(),
            idempotency_key: "destination-approve-wrong-origin-0001".to_owned(),
        })
        .expect_err("origin mismatch must fail closed");
    assert!(matches!(wrong_origin, KernelError::Conflict(_)));

    let approve_command = ApproveDestinationProfileCommand {
        schema_version: APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA.to_owned(),
        expected_engagement_generation: service.state().expect("state").generation,
        profile_id: frozen.profile.profile_id.clone(),
        profile_version: frozen.profile.profile_version.clone(),
        profile_hash: frozen.profile.profile_hash.clone(),
        origin_binding_hash: frozen.origin_binding_hash.clone(),
        actor: "Destination approver".to_owned(),
        reason: "Approve only local mapping and validation use.".to_owned(),
        authority_acknowledgement: LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT.to_owned(),
        idempotency_key: "destination-approve-profile-0001".to_owned(),
    };
    let approved = service
        .destination_service()
        .approve_destination_profile(&approve_command)
        .expect("approve exact destination profile");
    assert!(approved.profile.approval.is_some());
    assert_eq!(
        approved.engagement_generation,
        service.state().expect("post-approval state").generation
    );
    let candidate_after_approval = service
        .destination_service()
        .workshop()
        .expect("workshop with approved candidate")
        .reviewed_candidate
        .expect("approved reviewed candidate");
    assert_eq!(candidate_after_approval.profile, approved.profile);
    assert_eq!(
        candidate_after_approval.origin_binding_hash,
        frozen.origin_binding_hash
    );
    drop(service);
    let service = reopen_engagement(temporary.path());
    assert_eq!(
        service
            .destination_service()
            .workshop()
            .expect("reopen approved candidate")
            .reviewed_candidate
            .expect("approved candidate after reopen")
            .profile,
        approved.profile
    );
    assert_eq!(
        service
            .destination_service()
            .approve_destination_profile(&approve_command)
            .expect("exact approval retry"),
        approved
    );
    let mut changed_approval_retry = approve_command.clone();
    changed_approval_retry.reason = "Same approval key with different bytes must fail.".to_owned();
    let changed_approval_retry = service
        .destination_service()
        .approve_destination_profile(&changed_approval_retry)
        .expect_err("changed approval retry must conflict");
    assert!(matches!(changed_approval_retry, KernelError::Conflict(_)));

    let destination_first = service.journey().expect("destination-first journey");
    assert_eq!(
        stage(&destination_first, JourneyStageId::Destination).status,
        JourneyStageStatus::Completed
    );
    assert_eq!(
        stage(&destination_first, JourneyStageId::Source).status,
        JourneyStageStatus::Current
    );
    assert_eq!(
        stage(&destination_first, JourneyStageId::Mapping).status,
        JourneyStageStatus::Locked
    );
    assert!(
        stage(&destination_first, JourneyStageId::Mapping)
            .unavailable_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("sample-free profile"))
    );

    let revised = import_edmx(
        &service,
        &workspace,
        "fixtures/metadata/sanitized-sales-v2-revised.edmx",
        "revised",
    );
    assert_ne!(revised.snapshot_sha256, original.snapshot_sha256);
    assert_eq!(revised.property_count, original.property_count + 1);

    let rebase_command = RebaseDestinationDraftCommand {
        schema_version: REBASE_DRAFT_COMMAND_SCHEMA.to_owned(),
        expected_engagement_generation: service.state().expect("state").generation,
        draft_id: frozen.draft_id.clone(),
        expected_draft_generation: frozen.draft_generation,
        expected_draft_hash: frozen.draft_hash.clone(),
        target_catalogue_authority_id: revised.catalogue_authority_id.clone(),
        expected_target_catalogue_hash: revised.catalogue_hash.clone(),
        actor: "Destination practitioner".to_owned(),
        reason: "Rebase the frozen decisions onto the exact revised snapshot.".to_owned(),
        idempotency_key: "destination-rebase-revised-edmx-0001".to_owned(),
    };
    let rebase = service
        .destination_service()
        .rebase_draft(&rebase_command)
        .expect("rebase frozen destination draft");
    assert_eq!(
        rebase.engagement_generation,
        service.state().expect("post-rebase state").generation
    );
    assert_eq!(
        service
            .destination_service()
            .rebase_draft(&rebase_command)
            .expect("exact rebase retry"),
        rebase
    );
    let mut changed_rebase_retry = rebase_command.clone();
    changed_rebase_retry.reason = "Same rebase key with different bytes must fail.".to_owned();
    let changed_rebase_retry = service
        .destination_service()
        .rebase_draft(&changed_rebase_retry)
        .expect_err("changed rebase retry must conflict");
    assert!(matches!(changed_rebase_retry, KernelError::Conflict(_)));
    assert!(rebase.counts.get("UNCHANGED").copied().unwrap_or(0) >= 11);
    assert_eq!(
        rebase
            .counts
            .get("MANUAL_POTENTIAL_MATCH")
            .copied()
            .unwrap_or(0),
        1
    );
    let added_count = rebase.counts.get("ADDED").copied().unwrap_or(0);

    let rebased_root = rebase
        .draft
        .snapshot
        .entities
        .iter()
        .find(|entity| entity.root)
        .expect("rebased root");
    let unchanged_purchase_order = rebased_root
        .fields
        .iter()
        .find(|field| field.field_id == "purchaseOrderByCustomer")
        .expect("preserved optional field");
    assert_eq!(
        unchanged_purchase_order.state,
        DestinationFieldState::Active
    );
    assert_eq!(
        unchanged_purchase_order.diff_state,
        Some(DestinationDiffState::Unchanged)
    );
    let manual_match = rebased_root
        .fields
        .iter()
        .find(|field| field.field_id == "extensionReference")
        .expect("manual potential match");
    assert_eq!(
        manual_match.diff_state,
        Some(DestinationDiffState::ManualPotentialMatch)
    );
    assert_eq!(manual_match.state, DestinationFieldState::NeedsReview);
    assert!(manual_match.technical_property.is_none());
    assert!(manual_match.manual_supplement.is_some());

    let mut linked_snapshot = rebase.draft.snapshot.clone();
    let linked = linked_snapshot
        .entities
        .iter_mut()
        .find(|entity| entity.root)
        .and_then(|entity| {
            entity
                .fields
                .iter_mut()
                .find(|field| field.field_id == "extensionReference")
        })
        .expect("manual supplement to link");
    linked.technical_property = Some(format!("{ROOT_TECHNICAL_ENTITY}.extensionReference"));
    linked.state = DestinationFieldState::Active;
    linked.diff_state = Some(DestinationDiffState::Unchanged);
    let linked = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: rebase.draft.draft_id.clone(),
            expected_draft_generation: rebase.draft.generation,
            expected_draft_hash: rebase.draft.draft_hash.clone(),
            snapshot: linked_snapshot,
            actor: "Destination practitioner".to_owned(),
            reason: "Explicitly link the preserved canonical field to the revised EDMX property."
                .to_owned(),
            idempotency_key: "destination-link-manual-supplement-0001".to_owned(),
        })
        .expect("link manual supplement to revised EDMX");
    assert_eq!(linked.freeze_blockers, [] as [std::string::String; 0]);
    let linked_field = linked
        .snapshot
        .entities
        .iter()
        .find(|entity| entity.root)
        .and_then(|entity| {
            entity
                .fields
                .iter()
                .find(|field| field.field_id == "extensionReference")
        })
        .expect("linked extension field");
    assert!(linked_field.manual_supplement.is_some());
    assert_eq!(
        linked_field.technical_property.as_deref(),
        Some("Synthetic.VendorExtended.Sales.SalesOrder.extensionReference")
    );
    assert_eq!(
        service
            .destination_service()
            .start_draft(&start_command)
            .expect("start retry after later saves and freeze"),
        draft,
        "START_DRAFT retry must return its original immutable revision"
    );
    assert_eq!(
        service
            .destination_service()
            .save_draft(&save_command)
            .expect("save retry after later freeze and rebase"),
        saved,
        "SAVE_DRAFT retry must return its original immutable revision"
    );
    assert_eq!(
        service
            .destination_service()
            .rebase_draft(&rebase_command)
            .expect("rebase retry after later save"),
        rebase,
        "REBASE_DRAFT retry must return its original immutable result draft"
    );

    let integrity = integrity_check(temporary.path()).expect("global integrity replay");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
    assert!(integrity.checked_metadata_snapshots >= 2);
    assert!(integrity.checked_catalogues >= 2);
    let connection =
        Connection::open(temporary.path().join("state.sqlite3")).expect("open authority database");
    connection
        .execute_batch(
            "DROP TRIGGER destination_profile_origin_bindings_no_update;
             UPDATE destination_profile_origin_bindings
             SET canonical_origin_json = '{\"tampered\":true}'
             WHERE profile_id = 'synthetic.workspace.sales-document'
               AND profile_version = '1.0.0';",
        )
        .expect("simulate origin tamper");
    drop(connection);
    let tampered = integrity_check(temporary.path()).expect("tamper integrity replay");
    assert!(!tampered.ok);
    assert!(
        tampered
            .issues
            .iter()
            .any(|issue| issue.contains("destination") || issue.contains("origin"))
    );
    assert!(
        service.destination_service().workshop().is_err(),
        "active destination read must fail closed after origin tamper"
    );
    assert_eq!(
        added_count, 1,
        "only the extension introduced by the revised EDMX is structurally ADDED"
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn manual_origin_uses_the_same_bounded_workshop_and_exact_retry_contract() {
    let temporary = TempDir::new().expect("temporary workspace");
    let (service, _) = create_workspace_and_engagement(temporary.path());
    let initial_generation = service.state().expect("initial state").generation;
    let command = CreateManualCatalogueCommand {
        schema_version: CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA.to_owned(),
        expected_engagement_generation: initial_generation,
        artifact_label: "Practitioner-authored customer destination".to_owned(),
        actor: "Destination practitioner".to_owned(),
        reason: "No suitable technical artifact exists for this bounded synthetic scope."
            .to_owned(),
        entities: vec![ManualEntityDefinition {
            namespace: "Synthetic.Manual".to_owned(),
            name: "Customer".to_owned(),
            properties: vec![
                ManualPropertyDefinition {
                    name: "CustomerId".to_owned(),
                    edm_type: "Edm.String".to_owned(),
                    nullable: false,
                    max_length: Some(20),
                    key_candidate: true,
                },
                ManualPropertyDefinition {
                    name: "DisplayName".to_owned(),
                    edm_type: "Edm.String".to_owned(),
                    nullable: false,
                    max_length: Some(80),
                    key_candidate: false,
                },
                ManualPropertyDefinition {
                    name: "Notes".to_owned(),
                    edm_type: "Edm.String".to_owned(),
                    nullable: true,
                    max_length: Some(256),
                    key_candidate: false,
                },
            ],
        }],
        idempotency_key: "destination-manual-catalogue-0001".to_owned(),
    };
    let created = service
        .destination_service()
        .create_manual_catalogue(&command)
        .expect("create manual catalogue");
    assert_eq!(created.origin, "MANUAL");
    assert!(created.snapshot_id.is_none());
    assert!(created.snapshot_sha256.is_none());
    assert!(created.odata_generation.is_none());
    assert!(created.system.is_none());
    assert!(created.client_or_tenant.is_none());
    assert!(created.release.is_none());
    assert_eq!(
        created.manual_actor.as_deref(),
        Some("Destination practitioner")
    );
    assert_eq!(
        created.manual_reason.as_deref(),
        Some("No suitable technical artifact exists for this bounded synthetic scope.")
    );
    assert_eq!(created.namespace_count, 1);
    assert_eq!(created.entity_count, 1);
    assert_eq!(created.property_count, 3);
    assert_eq!(created.compatibility.unsupported_irrelevant_count, 0);
    assert_eq!(created.compatibility.unresolved_selected_scope_count, 0);
    drop(service);
    let service = reopen_engagement(temporary.path());
    let reopened_manual = service
        .destination_service()
        .workshop()
        .expect("reopen manual destination workshop")
        .latest_artifact
        .expect("reopened manual destination");
    assert_eq!(reopened_manual, created);
    assert!(reopened_manual.system.is_none());
    assert!(reopened_manual.client_or_tenant.is_none());
    assert!(reopened_manual.release.is_none());
    assert_eq!(
        reopened_manual.manual_actor.as_deref(),
        Some("Destination practitioner")
    );
    assert_eq!(
        reopened_manual.manual_reason.as_deref(),
        Some("No suitable technical artifact exists for this bounded synthetic scope.")
    );

    let exact_retry = service
        .destination_service()
        .create_manual_catalogue(&command);
    if let Ok(replayed) = &exact_retry {
        assert_eq!(replayed, &created);
    }

    let mut stale = command.clone();
    stale.idempotency_key = "destination-manual-catalogue-stale-0001".to_owned();
    stale.artifact_label = "A conflicting stale manual destination".to_owned();
    let stale = service
        .destination_service()
        .create_manual_catalogue(&stale)
        .expect_err("new stale command must fail");
    assert!(matches!(stale, KernelError::Conflict(_)));

    let entity_page = service
        .destination_service()
        .catalogue_entities(&created.catalogue_authority_id, Some("customer"), 10, None)
        .expect("manual entity page");
    assert_eq!(entity_page.items.len(), 1);
    assert_eq!(
        entity_page.items[0].technical_entity,
        "Synthetic.Manual.Customer"
    );

    let draft = service
        .destination_service()
        .start_draft(&StartDestinationDraftCommand {
            schema_version: START_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            catalogue_authority_id: created.catalogue_authority_id.clone(),
            expected_catalogue_hash: created.catalogue_hash.clone(),
            root_technical_entity: "Synthetic.Manual.Customer".to_owned(),
            child_technical_entities: Vec::new(),
            profile_id: "synthetic.workspace.manual-customer".to_owned(),
            title: "Practitioner-authored customer destination".to_owned(),
            owner: "Destination practitioner".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Use the common governed profile workshop.".to_owned(),
            idempotency_key: "destination-start-manual-draft-0001".to_owned(),
        })
        .expect("start manual-origin draft");
    assert_eq!(draft.snapshot.entities.len(), 1);
    assert_eq!(draft.snapshot.entities[0].fields.len(), 2);
    assert!(
        draft.snapshot.entities[0]
            .fields
            .iter()
            .all(|field| field.state == DestinationFieldState::NeedsReview)
    );
    let available = service
        .destination_service()
        .catalogue_properties(
            &created.catalogue_authority_id,
            "Synthetic.Manual.Customer",
            None,
            Some(DestinationFieldState::Available),
            10,
            None,
        )
        .expect("available manual properties");
    assert_eq!(available.items.len(), 1);
    assert_eq!(available.items[0].name, "Notes");

    let integrity = integrity_check(temporary.path()).expect("manual-origin integrity");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
    assert_eq!(exact_retry.expect("exact manual-catalogue retry"), created);
}

#[test]
#[allow(clippy::too_many_lines)]
fn v4_inventory_is_scope_neutral_and_receiving_recovery_is_fail_closed() {
    let v4_temporary = TempDir::new().expect("V4 workspace");
    let (v4_service, v4_workspace) = create_workspace_and_engagement(v4_temporary.path());
    let v4 = import_edmx(
        &v4_service,
        &v4_workspace,
        "fixtures/metadata/sanitized-sales-v4.edmx",
        "v4",
    );
    assert_eq!(v4.odata_generation.as_deref(), Some("V4"));
    assert_eq!(v4.entity_count, 4);
    assert_eq!(v4.compatibility.unresolved_selected_scope_count, 0);
    assert!(v4.compatibility.unsupported_inventory_count >= 12);
    assert_eq!(v4.compatibility.unsupported_irrelevant_count, 0);
    assert!(v4.compatibility.scoped_completeness_hash.is_none());
    assert!(v4.compatibility.findings.iter().all(|finding| {
        !finding.blocking
            && matches!(
                finding.classification.as_str(),
                "SCOPE_NEUTRAL_INVENTORY" | "NON_STRUCTURAL_WARNING"
            )
    }));
    let finding_codes = v4
        .compatibility
        .findings
        .iter()
        .map(|finding| finding.code.as_str())
        .collect::<std::collections::HashSet<_>>();
    for expected in [
        "REFERENCE",
        "COMPLEXTYPE",
        "INHERITANCE",
        "ALIAS",
        "REFERENTIALCONSTRAINT",
        "ENUMTYPE",
        "TYPEDEFINITION",
        "ACTION",
        "FUNCTION",
        "CONTAINMENT",
        "ANNOTATION",
        "NAVIGATIONBINDING",
    ] {
        assert!(
            finding_codes.contains(expected),
            "missing V4 inventory class {expected}"
        );
    }
    let broad_report = serde_json::to_string(&v4.compatibility).expect("serialize V4 report");
    assert!(!broad_report.contains("example.invalid"));
    assert!(!broad_report.contains("Synthetic annotation evidence only"));
    let v4_integrity = integrity_check(v4_temporary.path()).expect("V4 integrity");
    assert!(
        v4_integrity.ok,
        "integrity issues: {:?}",
        v4_integrity.issues
    );

    let interrupted_temporary = TempDir::new().expect("interrupted workspace");
    let (service, workspace) = create_workspace_and_engagement(interrupted_temporary.path());
    let generation = service.state().expect("engagement state").generation;
    let key = "destination-interrupted-upload-0001";
    let reservation = service
        .destination_service()
        .reserve_edmx(&ReserveEdmxCommand {
            schema_version: RESERVE_EDMX_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: generation,
            expected_workspace_id: workspace.workspace_id,
            expected_workspace_descriptor_hash: workspace.descriptor_hash,
            source_name: "interrupted.edmx".to_owned(),
            artifact_label: "Interrupted synthetic destination".to_owned(),
            system: None,
            client_or_tenant: None,
            release: None,
            idempotency_key: key.to_owned(),
        })
        .expect("reserve interrupted EDMX");
    let partial_path = reservation.controlled_path.clone();
    let mut upload = service
        .destination_service()
        .open_edmx_upload(&reservation.attempt_id, key)
        .expect("open interrupted upload");
    upload
        .write_all(b"<?xml version=\"1.0\"?><partial>")
        .expect("write interrupted bytes");
    drop(upload);
    assert!(partial_path.exists());
    drop(service);

    let service = reopen_engagement(interrupted_temporary.path());
    service.recover().expect("recover interrupted destination");
    assert!(!partial_path.exists());
    let connection = Connection::open(interrupted_temporary.path().join("state.sqlite3"))
        .expect("open recovered database");
    let (state, error_code): (String, Option<String>) = connection
        .query_row(
            "SELECT state, error_code
             FROM destination_artifact_attempts WHERE attempt_id = ?1",
            [&reservation.attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("recovered artifact attempt");
    assert_eq!(state, "INTERRUPTED");
    assert_eq!(error_code.as_deref(), Some("INTERRUPTED_RECOVERED"));
    drop(connection);
    let reopen = service
        .destination_service()
        .open_edmx_upload(&reservation.attempt_id, key)
        .expect_err("interrupted upload cannot reopen");
    assert!(matches!(reopen, KernelError::Conflict(_)));
    let recovered = service
        .destination_service()
        .workshop()
        .expect("recovered workshop");
    assert!(recovered.latest_artifact.is_none());
    let recovered_integrity =
        integrity_check(interrupted_temporary.path()).expect("recovered integrity");
    assert!(
        recovered_integrity.ok,
        "integrity issues: {:?}",
        recovered_integrity.issues
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn removed_active_property_requires_explicit_deactivation_before_new_profile_freeze() {
    let temporary = TempDir::new().expect("temporary workspace");
    let (service, workspace) = create_workspace_and_engagement(temporary.path());
    let (original, _, frozen, approved) =
        create_approved_original(&service, &workspace, "removed-base");
    let old_profile = approved.profile.clone();

    let original_bytes =
        fs::read_to_string(repository_path("fixtures/metadata/sanitized-sales-v2.edmx"))
            .expect("read original EDMX");
    let removed_property = concat!(
        "        <Property Name=\"PurchaseOrderByCustomer\" Type=\"Edm.String\" ",
        "Nullable=\"true\" MaxLength=\"35\" />\n"
    );
    assert_eq!(original_bytes.matches(removed_property).count(), 1);
    let revised_bytes = original_bytes.replace(removed_property, "");
    let revised = import_destination_bytes(
        &service,
        &workspace,
        revised_bytes.as_bytes(),
        "removed-property",
    );
    assert_eq!(revised.property_count + 1, original.property_count);

    let rebase = service
        .destination_service()
        .rebase_draft(&RebaseDestinationDraftCommand {
            schema_version: REBASE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: frozen.draft_id.clone(),
            expected_draft_generation: frozen.draft_generation,
            expected_draft_hash: frozen.draft_hash.clone(),
            target_catalogue_authority_id: revised.catalogue_authority_id.clone(),
            expected_target_catalogue_hash: revised.catalogue_hash.clone(),
            actor: "Destination practitioner".to_owned(),
            reason: "Review the exact removal in the revised destination evidence.".to_owned(),
            idempotency_key: "destination-rebase-removed-property-0001".to_owned(),
        })
        .expect("rebase removed-property destination");
    assert_eq!(rebase.counts.get("REMOVED").copied().unwrap_or(0), 1);
    assert_eq!(rebase.counts.get("ADDED").copied().unwrap_or(0), 0);
    let removed = rebase
        .draft
        .snapshot
        .entities
        .iter()
        .find(|entity| entity.root)
        .and_then(|entity| {
            entity
                .fields
                .iter()
                .find(|field| field.field_id == "purchaseOrderByCustomer")
        })
        .expect("preserved removed active field");
    assert_eq!(removed.state, DestinationFieldState::NeedsReview);
    assert_eq!(removed.diff_state, Some(DestinationDiffState::Removed));
    assert_eq!(
        removed.technical_property.as_deref(),
        Some("Synthetic.VendorExtended.Sales.SalesOrder.PurchaseOrderByCustomer")
    );
    assert!(rebase.draft.freeze_blockers.iter().any(|blocker| {
        blocker.contains("salesOrder.purchaseOrderByCustomer still needs practitioner review")
    }));

    let blocked = service
        .destination_service()
        .freeze_draft(&FreezeDestinationDraftCommand {
            schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: rebase.draft.draft_id.clone(),
            expected_draft_generation: rebase.draft.generation,
            expected_draft_hash: rebase.draft.draft_hash.clone(),
            profile_version: "1.1.0".to_owned(),
            acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
            actor: "Destination reviewer".to_owned(),
            reason: "A removed active field must be resolved first.".to_owned(),
            idempotency_key: "destination-freeze-unresolved-removal-0001".to_owned(),
        })
        .expect_err("unresolved removal must block freeze");
    assert!(matches!(blocked, KernelError::ProfileSemantic(_)));

    let mut deactivated = rebase.draft.snapshot.clone();
    let root = deactivated
        .entities
        .iter_mut()
        .find(|entity| entity.root)
        .expect("rebased root");
    let deactivated_field = root
        .fields
        .iter_mut()
        .find(|field| field.field_id == "purchaseOrderByCustomer")
        .expect("removed field to deactivate");
    deactivated_field.state = DestinationFieldState::Available;
    let saved = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: rebase.draft.draft_id.clone(),
            expected_draft_generation: rebase.draft.generation,
            expected_draft_hash: rebase.draft.draft_hash.clone(),
            snapshot: deactivated,
            actor: "Destination practitioner".to_owned(),
            reason: "Explicitly deactivate the destination field removed from the revised EDMX."
                .to_owned(),
            idempotency_key: "destination-deactivate-removed-property-0001".to_owned(),
        })
        .expect("save explicit removal deactivation");
    assert_eq!(saved.freeze_blockers, [] as [std::string::String; 0]);
    assert!(
        saved
            .snapshot
            .entities
            .iter()
            .flat_map(|entity| &entity.fields)
            .find(|field| field.field_id == "purchaseOrderByCustomer")
            .is_some_and(|field| {
                field.state == DestinationFieldState::Available
                    && field.diff_state == Some(DestinationDiffState::Removed)
                    && field.technical_property.as_deref()
                        == Some("Synthetic.VendorExtended.Sales.SalesOrder.PurchaseOrderByCustomer")
            })
    );

    let replacement = service
        .destination_service()
        .freeze_draft(&FreezeDestinationDraftCommand {
            schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: saved.draft_id.clone(),
            expected_draft_generation: saved.generation,
            expected_draft_hash: saved.draft_hash.clone(),
            profile_version: "1.1.0".to_owned(),
            acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
            actor: "Destination reviewer".to_owned(),
            reason: "Freeze the resolved revised destination profile.".to_owned(),
            idempotency_key: "destination-freeze-resolved-removal-0001".to_owned(),
        })
        .expect("freeze replacement destination profile");
    assert_ne!(replacement.profile.profile_hash, old_profile.profile_hash);
    let workshop_before_replacement_approval = service
        .destination_service()
        .workshop()
        .expect("workshop with replacement reviewed candidate");
    assert_eq!(
        workshop_before_replacement_approval
            .active_destination
            .as_ref()
            .map(|active| active.profile_hash.as_str()),
        Some(old_profile.profile_hash.as_str())
    );
    assert_eq!(
        workshop_before_replacement_approval.reviewed_candidate,
        Some(replacement.clone())
    );
    let replacement_approved = service
        .destination_service()
        .approve_destination_profile(&ApproveDestinationProfileCommand {
            schema_version: APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            profile_id: replacement.profile.profile_id.clone(),
            profile_version: replacement.profile.profile_version.clone(),
            profile_hash: replacement.profile.profile_hash.clone(),
            origin_binding_hash: replacement.origin_binding_hash.clone(),
            actor: "Destination approver".to_owned(),
            reason: "Approve the resolved revision for local mapping and validation only."
                .to_owned(),
            authority_acknowledgement: LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT.to_owned(),
            idempotency_key: "destination-approve-resolved-removal-0001".to_owned(),
        })
        .expect("approve replacement destination profile");
    assert!(replacement_approved.profile.approval.is_some());

    let old_after = migration_stream::profile_registry::inspect_profile(
        temporary.path(),
        &old_profile.profile_id,
        &old_profile.profile_version,
    )
    .expect("replay old immutable profile");
    assert_eq!(old_after, old_profile);
    let workshop = service
        .destination_service()
        .workshop()
        .expect("workshop after replacement approval");
    assert_eq!(
        workshop
            .active_destination
            .as_ref()
            .map(|active| active.profile_hash.as_str()),
        Some(replacement_approved.profile.profile_hash.as_str()),
        "an exact revised profile may become active when no mapping or validation authority exists"
    );
    let state_after_replacement = service.state().expect("state after replacement approval");
    assert_eq!(state_after_replacement.mapping_stage, "NOT_PREPARED");
    assert!(state_after_replacement.active.mapping_id.is_none());
    assert!(state_after_replacement.active.validation_run_id.is_none());
    assert_eq!(
        workshop
            .reviewed_candidate
            .as_ref()
            .map(|candidate| &candidate.profile),
        Some(&replacement_approved.profile)
    );

    drop(service);
    let reopened = reopen_engagement(temporary.path());
    let reopened_workshop = reopened
        .destination_service()
        .workshop()
        .expect("reopen revised destination workshop");
    assert_eq!(
        reopened_workshop
            .active_destination
            .as_ref()
            .map(|active| active.profile_hash.as_str()),
        Some(replacement_approved.profile.profile_hash.as_str())
    );
    assert_eq!(
        reopened_workshop
            .reviewed_candidate
            .as_ref()
            .map(|candidate| &candidate.profile),
        Some(&replacement_approved.profile)
    );
    let integrity = integrity_check(temporary.path()).expect("removal integrity replay");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
fn selected_structural_unsupported_finding_blocks_freeze_even_when_acknowledged() {
    let temporary = TempDir::new().expect("selected unsupported workspace");
    let (service, workspace) = create_workspace_and_engagement(temporary.path());
    let original = fs::read_to_string(repository_path("fixtures/metadata/sanitized-sales-v4.edmx"))
        .expect("read V4 fixture");
    let optional_audit = concat!(
        "<Property Name=\"AuditNote\" Type=\"Edm.String\" ",
        "Nullable=\"true\" MaxLength=\"80\" />"
    );
    assert_eq!(original.matches(optional_audit).count(), 1);
    let selected_fixture = original.replace(
        optional_audit,
        concat!(
            "<Property Name=\"AuditNote\" Type=\"Edm.String\" ",
            "Nullable=\"false\" MaxLength=\"80\" />"
        ),
    );
    let artifact = import_destination_bytes(
        &service,
        &workspace,
        selected_fixture.as_bytes(),
        "selected-inheritance",
    );
    let inheritance = artifact
        .compatibility
        .findings
        .iter()
        .find(|finding| {
            finding.code == "INHERITANCE"
                && finding.technical_location == "Synthetic.ODataV4.Sales.DerivedAudit"
        })
        .expect("selected inheritance finding");
    assert!(
        !inheritance.blocking,
        "artifact report remains scope-neutral"
    );

    let draft = start_single_entity_draft(
        &service,
        &artifact,
        "Synthetic.ODataV4.Sales.DerivedAudit",
        "synthetic.workspace.v4-derived-audit",
        "selected-inheritance",
    );
    assert_eq!(draft.snapshot.entities[0].fields.len(), 1);
    let mut selected = confirm_all_suggested_fields(&draft.snapshot);
    selected.compatibility_acknowledgements = vec![CompatibilityAcknowledgement {
        finding_hash: inheritance.finding_hash.clone(),
        actor: "Destination practitioner".to_owned(),
        reason: "Acknowledgement cannot convert unsupported inheritance into authority.".to_owned(),
    }];
    let saved = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id.clone(),
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash.clone(),
            snapshot: selected,
            actor: "Destination practitioner".to_owned(),
            reason: "Confirm the field while preserving selected-scope uncertainty.".to_owned(),
            idempotency_key: "destination-save-selected-inheritance-0001".to_owned(),
        })
        .expect("save selected inheritance draft");
    assert!(saved.freeze_blockers.iter().any(|blocker| {
        let blocker = blocker.to_ascii_lowercase();
        blocker.contains("inheritance") && blocker.contains("derivedaudit")
    }));

    let blocked = service
        .destination_service()
        .freeze_draft(&FreezeDestinationDraftCommand {
            schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: saved.draft_id,
            expected_draft_generation: saved.generation,
            expected_draft_hash: saved.draft_hash,
            profile_version: "1.0.0".to_owned(),
            acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
            actor: "Destination reviewer".to_owned(),
            reason: "Selected structural inheritance must remain unresolved.".to_owned(),
            idempotency_key: "destination-freeze-selected-inheritance-0001".to_owned(),
        })
        .expect_err("selected unsupported inheritance must block freeze");
    let detail = blocked.to_string().to_ascii_lowercase();
    assert!(detail.contains("inheritance"));
    assert!(detail.contains("derivedaudit"));
    let integrity = integrity_check(temporary.path()).expect("selected-scope integrity replay");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
fn unrelated_unsupported_findings_do_not_block_selected_primitive_scope() {
    let temporary = TempDir::new().expect("irrelevant unsupported workspace");
    let (service, workspace) = create_workspace_and_engagement(temporary.path());
    let artifact = import_edmx(
        &service,
        &workspace,
        "fixtures/metadata/sanitized-sales-v4.edmx",
        "irrelevant-unsupported",
    );
    assert!(
        artifact
            .compatibility
            .findings
            .iter()
            .any(|finding| finding.code == "COMPLEXTYPE")
    );
    assert!(
        artifact
            .compatibility
            .findings
            .iter()
            .any(|finding| finding.code == "INHERITANCE")
    );
    let draft = start_single_entity_draft(
        &service,
        &artifact,
        "Synthetic.ODataV4.Sales.Attachment",
        "synthetic.workspace.v4-attachment",
        "irrelevant-unsupported",
    );
    assert_eq!(draft.snapshot.entities[0].fields.len(), 1);
    let saved = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id.clone(),
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash.clone(),
            snapshot: confirm_all_suggested_fields(&draft.snapshot),
            actor: "Destination practitioner".to_owned(),
            reason: "Confirm only the primitive Attachment scope.".to_owned(),
            idempotency_key: "destination-save-irrelevant-unsupported-0001".to_owned(),
        })
        .expect("save primitive selected scope");
    assert!(
        saved.freeze_blockers.is_empty(),
        "unrelated findings must remain nonblocking: {:?}",
        saved.freeze_blockers
    );
    let frozen = service
        .destination_service()
        .freeze_draft(&FreezeDestinationDraftCommand {
            schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: saved.draft_id,
            expected_draft_generation: saved.generation,
            expected_draft_hash: saved.draft_hash,
            profile_version: "1.0.0".to_owned(),
            acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
            actor: "Destination reviewer".to_owned(),
            reason: "Freeze the bounded primitive scope only.".to_owned(),
            idempotency_key: "destination-freeze-irrelevant-unsupported-0001".to_owned(),
        })
        .expect("freeze scope unaffected by unrelated unsupported constructs");
    assert_eq!(frozen.profile.declared_status, "REVIEWED_DRAFT");
    let integrity = integrity_check(temporary.path()).expect("irrelevant-scope integrity replay");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn selected_v2_referential_constraint_requires_an_exact_operator_resolution() {
    let temporary = TempDir::new().expect("referential-constraint workspace");
    let (service, workspace) = create_workspace_and_engagement(temporary.path());
    let artifact = import_edmx(
        &service,
        &workspace,
        "fixtures/metadata/sanitized-sales-v2.edmx",
        "relationship-scope",
    );
    let draft = service
        .destination_service()
        .start_draft(&StartDestinationDraftCommand {
            schema_version: START_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            catalogue_authority_id: artifact.catalogue_authority_id.clone(),
            expected_catalogue_hash: artifact.catalogue_hash.clone(),
            root_technical_entity: ROOT_TECHNICAL_ENTITY.to_owned(),
            child_technical_entities: vec![CHILD_TECHNICAL_ENTITY.to_owned()],
            profile_id: "synthetic.workspace.relationship-scope".to_owned(),
            title: "Relationship scope".to_owned(),
            owner: "Destination practitioner".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Review the exact selected relationship evidence.".to_owned(),
            idempotency_key: "destination-start-relationship-scope-0001".to_owned(),
        })
        .expect("start relationship-scope draft");
    let mut unresolved_snapshot = confirm_all_suggested_fields(&draft.snapshot);
    unresolved_snapshot.relationships = vec![reviewed_relationship(false)];
    let unresolved = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id.clone(),
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash.clone(),
            snapshot: unresolved_snapshot,
            actor: "Destination practitioner".to_owned(),
            reason: "Keep the selected relationship unresolved for proof.".to_owned(),
            idempotency_key: "destination-save-unresolved-relationship-0001".to_owned(),
        })
        .expect("save unresolved selected relationship");
    assert!(unresolved.freeze_blockers.iter().any(|blocker| {
        blocker
            .to_ascii_lowercase()
            .contains("unresolved referentialconstraint")
    }));
    let unresolved_report = service
        .destination_service()
        .workshop()
        .expect("unresolved relationship workshop")
        .latest_artifact
        .expect("scoped artifact")
        .compatibility;
    let unresolved_finding = unresolved_report
        .findings
        .iter()
        .find(|finding| finding.code == "REFERENTIALCONSTRAINT")
        .expect("referential constraint finding");
    assert!(unresolved_finding.blocking);
    assert_eq!(
        unresolved_finding.classification,
        "UNRESOLVED_SELECTED_SCOPE"
    );

    let mut resolved_snapshot = unresolved.snapshot.clone();
    resolved_snapshot.relationships[0].confirmed = true;
    let resolved = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: unresolved.draft_id.clone(),
            expected_draft_generation: unresolved.generation,
            expected_draft_hash: unresolved.draft_hash.clone(),
            snapshot: resolved_snapshot,
            actor: "Destination practitioner".to_owned(),
            reason: "Confirm the exact bounded relationship key tuple.".to_owned(),
            idempotency_key: "destination-save-resolved-relationship-0001".to_owned(),
        })
        .expect("save exact relationship resolution");
    assert!(
        resolved.freeze_blockers.is_empty(),
        "exact operator relationship must resolve the importer gap: {:?}",
        resolved.freeze_blockers
    );
    let resolved_report = service
        .destination_service()
        .workshop()
        .expect("resolved relationship workshop")
        .latest_artifact
        .expect("scoped artifact")
        .compatibility;
    let resolved_finding = resolved_report
        .findings
        .iter()
        .find(|finding| finding.code == "REFERENTIALCONSTRAINT")
        .expect("resolved referential constraint finding");
    assert!(!resolved_finding.blocking);
    assert_eq!(
        resolved_finding.classification,
        "SELECTED_SCOPE_OPERATOR_RESOLVED"
    );
    assert_ne!(
        unresolved_report.scoped_completeness_hash,
        resolved_report.scoped_completeness_hash
    );
    service
        .destination_service()
        .freeze_draft(&FreezeDestinationDraftCommand {
            schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: resolved.draft_id,
            expected_draft_generation: resolved.generation,
            expected_draft_hash: resolved.draft_hash,
            profile_version: "1.0.0".to_owned(),
            acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
            actor: "Destination reviewer".to_owned(),
            reason: "Freeze the exactly resolved relationship scope.".to_owned(),
            idempotency_key: "destination-freeze-resolved-relationship-0001".to_owned(),
        })
        .expect("freeze exact relationship resolution");
    let integrity = integrity_check(temporary.path()).expect("relationship integrity replay");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn freeze_rejects_stale_rules_noncanonical_source_keys_and_dotted_entity_ids() {
    let temporary = TempDir::new().expect("freeze-invariant workspace");
    let (service, workspace) = create_workspace_and_engagement(temporary.path());
    let artifact = import_edmx(
        &service,
        &workspace,
        "fixtures/metadata/sanitized-sales-v2.edmx",
        "freeze-invariants",
    );
    let draft = service
        .destination_service()
        .start_draft(&StartDestinationDraftCommand {
            schema_version: START_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            catalogue_authority_id: artifact.catalogue_authority_id.clone(),
            expected_catalogue_hash: artifact.catalogue_hash.clone(),
            root_technical_entity: ROOT_TECHNICAL_ENTITY.to_owned(),
            child_technical_entities: vec![CHILD_TECHNICAL_ENTITY.to_owned()],
            profile_id: "synthetic.workspace.freeze-invariants".to_owned(),
            title: "Freeze invariants".to_owned(),
            owner: "Destination practitioner".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Exercise exact destination freeze invariants.".to_owned(),
            idempotency_key: "destination-start-freeze-invariants-0001".to_owned(),
        })
        .expect("start freeze-invariant draft");

    let mut dotted = draft.snapshot.clone();
    dotted.entities[0].entity_id = "invalid.entity".to_owned();
    let dotted = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id.clone(),
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash.clone(),
            snapshot: dotted,
            actor: "Destination practitioner".to_owned(),
            reason: "A dotted entity ID must fail before rule targeting becomes ambiguous."
                .to_owned(),
            idempotency_key: "destination-save-dotted-entity-0001".to_owned(),
        })
        .expect_err("dotted canonical entity ID must fail");
    assert!(matches!(dotted, KernelError::PolicyInvalid(_)));

    let mut baseline = confirm_all_suggested_fields(&draft.snapshot);
    baseline.relationships = vec![reviewed_relationship(true)];
    let sales_order = baseline
        .entities
        .iter_mut()
        .find(|entity| entity.entity_id == ROOT_ENTITY)
        .and_then(|entity| {
            entity
                .fields
                .iter_mut()
                .find(|field| field.field_id == "salesOrder")
        })
        .expect("root sales order field");
    sales_order.confirm_max_length = true;
    assert_eq!(sales_order.max_length, Some(10));
    baseline.rules.push(DestinationRuleDraft {
        rule_id: "max-sales-order".to_owned(),
        severity: "error".to_owned(),
        kind: "max_length".to_owned(),
        target: format!("{ROOT_ENTITY}.salesOrder"),
        message: "Sales order must fit the reviewed exact maximum.".to_owned(),
        parameters: json!({"maximum": 10}),
    });
    let saved = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id.clone(),
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash.clone(),
            snapshot: baseline,
            actor: "Destination practitioner".to_owned(),
            reason: "Establish a fully aligned freeze baseline.".to_owned(),
            idempotency_key: "destination-save-freeze-baseline-0001".to_owned(),
        })
        .expect("save aligned freeze baseline");
    assert_eq!(saved.freeze_blockers, [] as [std::string::String; 0]);

    let mut stale = saved.snapshot.clone();
    let root = stale
        .entities
        .iter_mut()
        .find(|entity| entity.entity_id == ROOT_ENTITY)
        .expect("root entity");
    root.fields
        .iter_mut()
        .find(|field| field.field_id == "salesOrder")
        .expect("source relationship key")
        .key = false;
    root.fields
        .iter_mut()
        .find(|field| field.field_id == "salesOrderType")
        .expect("required decision")
        .business_required = false;
    stale
        .rules
        .iter_mut()
        .find(|rule| rule.rule_id == "max-sales-order")
        .expect("maximum rule")
        .parameters = json!({"maximum": 9});
    let stale = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: saved.draft_id.clone(),
            expected_draft_generation: saved.generation,
            expected_draft_hash: saved.draft_hash.clone(),
            snapshot: stale,
            actor: "Destination practitioner".to_owned(),
            reason: "Persist deliberate stale-rule and unsafe-key proof state.".to_owned(),
            idempotency_key: "destination-save-stale-freeze-invariants-0001".to_owned(),
        })
        .expect("save blocked freeze state");
    assert!(stale.freeze_blockers.iter().any(|blocker| {
        blocker.contains("required rule") && blocker.contains("active business-required")
    }));
    assert!(stale.freeze_blockers.iter().any(|blocker| {
        blocker.contains("max_length rule") && blocker.contains("exact confirmed bounded length")
    }));
    assert!(stale.freeze_blockers.iter().any(|blocker| {
        blocker.contains("source key") && blocker.contains("canonical entity key")
    }));
    let blocked = service
        .destination_service()
        .freeze_draft(&FreezeDestinationDraftCommand {
            schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: stale.draft_id,
            expected_draft_generation: stale.generation,
            expected_draft_hash: stale.draft_hash,
            profile_version: "1.0.0".to_owned(),
            acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
            actor: "Destination reviewer".to_owned(),
            reason: "Stale enforcement and unsafe relationship keys must not freeze.".to_owned(),
            idempotency_key: "destination-freeze-stale-invariants-0001".to_owned(),
        })
        .expect_err("stale enforcement state must fail freeze");
    assert!(matches!(blocked, KernelError::ProfileSemantic(_)));
    let integrity = integrity_check(temporary.path()).expect("freeze-invariant integrity replay");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn conditional_and_reference_rules_are_typed_entity_scoped_and_freeze_only_when_active() {
    let temporary = TempDir::new().expect("typed-rule workspace");
    let (service, workspace) = create_workspace_and_engagement(temporary.path());
    let artifact = import_edmx(
        &service,
        &workspace,
        "fixtures/metadata/sanitized-sales-v2.edmx",
        "typed-rules",
    );
    let draft = service
        .destination_service()
        .start_draft(&StartDestinationDraftCommand {
            schema_version: START_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            catalogue_authority_id: artifact.catalogue_authority_id,
            expected_catalogue_hash: artifact.catalogue_hash,
            root_technical_entity: ROOT_TECHNICAL_ENTITY.to_owned(),
            child_technical_entities: vec![CHILD_TECHNICAL_ENTITY.to_owned()],
            profile_id: "synthetic.workspace.typed-rules".to_owned(),
            title: "Typed destination rules".to_owned(),
            owner: "Destination practitioner".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Exercise typed conditional and reference obligations.".to_owned(),
            idempotency_key: "destination-start-typed-rules-0001".to_owned(),
        })
        .expect("start typed-rule draft");
    let mut reviewed = confirm_all_suggested_fields(&draft.snapshot);
    reviewed.relationships = vec![reviewed_relationship(true)];
    let root = reviewed
        .entities
        .iter()
        .find(|entity| entity.entity_id == ROOT_ENTITY)
        .expect("root entity");
    let condition_field = root
        .fields
        .iter()
        .find(|field| !field.key)
        .expect("non-key root condition field")
        .field_id
        .clone();
    let target_field = root
        .fields
        .iter()
        .find(|field| field.field_id != condition_field)
        .expect("distinct root target field")
        .field_id
        .clone();
    let child_field = reviewed
        .entities
        .iter()
        .find(|entity| entity.entity_id == CHILD_ENTITY)
        .and_then(|entity| entity.fields.first())
        .expect("child field")
        .field_id
        .clone();
    let conditional_rule = DestinationRuleDraft {
        rule_id: "conditional-root-target".to_owned(),
        severity: "error".to_owned(),
        kind: "conditional_required".to_owned(),
        target: format!("{ROOT_ENTITY}.{target_field}"),
        message: "The reviewed condition requires this destination field.".to_owned(),
        parameters: json!({
            "whenField": format!("{ROOT_ENTITY}.{condition_field}"),
            "equals": "STANDARD",
        }),
    };
    let reference_rule = DestinationRuleDraft {
        rule_id: "reference-root-target".to_owned(),
        severity: "error".to_owned(),
        kind: "reference_lookup".to_owned(),
        target: format!("{ROOT_ENTITY}.{condition_field}"),
        message: "The destination field must resolve against the reviewed reference table."
            .to_owned(),
        parameters: json!({
            "tableId": "document-type",
            "tableVersion": "1.0.0",
        }),
    };

    let save_error = |snapshot: DestinationDraftSnapshot, idempotency_key: &str| {
        service
            .destination_service()
            .save_draft(&SaveDestinationDraftCommand {
                schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
                expected_engagement_generation: service.state().expect("state").generation,
                draft_id: draft.draft_id.clone(),
                expected_draft_generation: draft.generation,
                expected_draft_hash: draft.draft_hash.clone(),
                snapshot,
                actor: "Destination practitioner".to_owned(),
                reason: "Reject malformed typed destination-rule authority.".to_owned(),
                idempotency_key: idempotency_key.to_owned(),
            })
            .expect_err("malformed typed rule must fail before draft mutation")
    };

    let mut cross_entity = reviewed.clone();
    let mut invalid = conditional_rule.clone();
    invalid.parameters["whenField"] = json!(format!("{CHILD_ENTITY}.{child_field}"));
    cross_entity.rules.push(invalid);
    let cross_entity = save_error(cross_entity, "destination-save-cross-entity-condition-0001");
    assert!(matches!(cross_entity, KernelError::ProfileSemantic(message)
            if message.contains("same entity")));

    let mut extra_parameter = reviewed.clone();
    let mut invalid = conditional_rule.clone();
    invalid.parameters["unexpected"] = json!("not-authority");
    extra_parameter.rules.push(invalid);
    let extra_parameter = save_error(
        extra_parameter,
        "destination-save-extra-conditional-parameter-0001",
    );
    assert!(
        matches!(extra_parameter, KernelError::ProfileSemantic(message)
            if message.contains("parameter keys must be exactly"))
    );

    let mut oversized_condition = reviewed.clone();
    let mut invalid = conditional_rule.clone();
    invalid.parameters["equals"] = json!("x".repeat(4097));
    oversized_condition.rules.push(invalid);
    let oversized_condition = save_error(
        oversized_condition,
        "destination-save-oversized-condition-0001",
    );
    assert!(
        matches!(oversized_condition, KernelError::ProfileSemantic(message)
            if message.contains("4096-byte bound"))
    );

    let mut noncanonical_table = reviewed.clone();
    let mut invalid = reference_rule.clone();
    invalid.parameters["tableId"] = json!("Document_Type");
    noncanonical_table.rules.push(invalid);
    let noncanonical_table = save_error(
        noncanonical_table,
        "destination-save-noncanonical-table-0001",
    );
    assert!(
        matches!(noncanonical_table, KernelError::ProfileSemantic(message)
            if message.contains("lowercase mapping identifier"))
    );

    let mut invalid_version = reviewed.clone();
    let mut invalid = reference_rule.clone();
    invalid.parameters["tableVersion"] = json!("1.0");
    invalid_version.rules.push(invalid);
    let invalid_version = save_error(
        invalid_version,
        "destination-save-invalid-table-version-0001",
    );
    assert!(
        matches!(invalid_version, KernelError::ProfileSemantic(message)
            if message.contains("numeric major.minor.patch"))
    );

    let mut conflicting_versions = reviewed.clone();
    conflicting_versions.rules.push(reference_rule.clone());
    let mut conflicting = reference_rule.clone();
    conflicting.rule_id = "reference-root-target-v2".to_owned();
    conflicting.parameters["tableVersion"] = json!("2.0.0");
    conflicting_versions.rules.push(conflicting);
    let conflicting_versions = save_error(
        conflicting_versions,
        "destination-save-conflicting-table-versions-0001",
    );
    assert!(
        matches!(conflicting_versions, KernelError::ProfileSemantic(message)
            if message.contains("conflicting table versions"))
    );

    let mut inactive = reviewed.clone();
    let condition = inactive
        .entities
        .iter_mut()
        .find(|entity| entity.entity_id == ROOT_ENTITY)
        .and_then(|entity| {
            entity
                .fields
                .iter_mut()
                .find(|field| field.field_id == condition_field)
        })
        .expect("condition field to deactivate");
    condition.state = DestinationFieldState::Available;
    condition.business_required = false;
    inactive.rules.retain(|rule| {
        !(rule.kind == "required" && rule.target == format!("{ROOT_ENTITY}.{condition_field}"))
    });
    inactive.rules.push(conditional_rule.clone());
    inactive.rules.push(reference_rule.clone());
    let blocked = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id.clone(),
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash.clone(),
            snapshot: inactive,
            actor: "Destination practitioner".to_owned(),
            reason: "Retain typed rules while their exact fields still need activation.".to_owned(),
            idempotency_key: "destination-save-inactive-typed-rules-0001".to_owned(),
        })
        .expect("save recoverable inactive typed-rule draft");
    assert!(blocked.freeze_blockers.iter().any(|blocker| {
        blocker.contains("conditional_required")
            && blocker.contains("active same-entity condition field")
    }));
    assert!(blocked.freeze_blockers.iter().any(|blocker| {
        blocker.contains("reference_lookup") && blocker.contains("active field")
    }));

    reviewed.rules.push(conditional_rule);
    reviewed.rules.push(reference_rule);
    let ready = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: blocked.draft_id.clone(),
            expected_draft_generation: blocked.generation,
            expected_draft_hash: blocked.draft_hash.clone(),
            snapshot: reviewed,
            actor: "Destination practitioner".to_owned(),
            reason: "Confirm exact active fields and typed destination-rule obligations."
                .to_owned(),
            idempotency_key: "destination-save-ready-typed-rules-0001".to_owned(),
        })
        .expect("save complete typed-rule draft");
    assert!(
        ready.freeze_blockers.is_empty(),
        "ready typed rules must be freezeable: {:?}",
        ready.freeze_blockers
    );
    service
        .destination_service()
        .freeze_draft(&FreezeDestinationDraftCommand {
            schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: ready.draft_id,
            expected_draft_generation: ready.generation,
            expected_draft_hash: ready.draft_hash,
            profile_version: "1.0.0".to_owned(),
            acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
            actor: "Destination reviewer".to_owned(),
            reason: "Freeze the exact typed destination-rule contract.".to_owned(),
            idempotency_key: "destination-freeze-ready-typed-rules-0001".to_owned(),
        })
        .expect("freeze exact typed destination-rule contract");
    let integrity = integrity_check(temporary.path()).expect("typed-rule integrity replay");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn technical_change_review_is_exact_bounded_and_cleared_by_the_next_rebase() {
    let temporary = TempDir::new().expect("technical-change workspace");
    let (service, workspace) = create_workspace_and_engagement(temporary.path());
    let (_, _, frozen, _) = create_approved_original(&service, &workspace, "technical-base");
    let original = fs::read_to_string(repository_path("fixtures/metadata/sanitized-sales-v2.edmx"))
        .expect("read original EDMX");
    let changed = original.replace(
        "Name=\"PurchaseOrderByCustomer\" Type=\"Edm.String\" Nullable=\"true\" MaxLength=\"35\"",
        "Name=\"PurchaseOrderByCustomer\" Type=\"Edm.String\" Nullable=\"true\" MaxLength=\"40\"",
    );
    assert_ne!(changed, original);
    let changed_artifact =
        import_destination_bytes(&service, &workspace, changed.as_bytes(), "technical-change");
    let rebase = service
        .destination_service()
        .rebase_draft(&RebaseDestinationDraftCommand {
            schema_version: REBASE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: frozen.draft_id.clone(),
            expected_draft_generation: frozen.draft_generation,
            expected_draft_hash: frozen.draft_hash.clone(),
            target_catalogue_authority_id: changed_artifact.catalogue_authority_id.clone(),
            expected_target_catalogue_hash: changed_artifact.catalogue_hash.clone(),
            actor: "Destination practitioner".to_owned(),
            reason: "Review one exact technical maximum change.".to_owned(),
            idempotency_key: "destination-rebase-technical-change-0001".to_owned(),
        })
        .expect("rebase technical change");
    assert_eq!(
        rebase
            .counts
            .get("TECHNICALLY_CHANGED")
            .copied()
            .unwrap_or(0),
        1
    );
    let changed_field = rebase
        .draft
        .snapshot
        .entities
        .iter()
        .find(|entity| entity.entity_id == ROOT_ENTITY)
        .and_then(|entity| {
            entity
                .fields
                .iter()
                .find(|field| field.field_id == "purchaseOrderByCustomer")
        })
        .expect("changed destination field");
    assert_eq!(
        changed_field.diff_state,
        Some(DestinationDiffState::TechnicallyChanged)
    );
    assert_eq!(changed_field.state, DestinationFieldState::NeedsReview);
    assert!(changed_field.technical_change_review.is_none());

    let mut fabricated = rebase.draft.snapshot.clone();
    let fabricated_field = fabricated
        .entities
        .iter_mut()
        .find(|entity| entity.entity_id == ROOT_ENTITY)
        .and_then(|entity| {
            entity
                .fields
                .iter_mut()
                .find(|field| field.field_id == "salesOrderType")
        })
        .expect("unchanged field");
    fabricated_field.diff_state = Some(DestinationDiffState::TechnicallyChanged);
    fabricated_field.state = DestinationFieldState::Active;
    fabricated_field.technical_change_review = Some(TechnicalChangeReviewEvidence {
        decision: TechnicalChangeDecision::AcceptRevisedTechnicalEvidence,
        actor: "Destination practitioner".to_owned(),
        reason: "This fabricated review must not match immutable diff evidence.".to_owned(),
    });
    let fabricated = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: rebase.draft.draft_id.clone(),
            expected_draft_generation: rebase.draft.generation,
            expected_draft_hash: rebase.draft.draft_hash.clone(),
            snapshot: fabricated,
            actor: "Destination practitioner".to_owned(),
            reason: "Attempt fabricated revised-technical evidence.".to_owned(),
            idempotency_key: "destination-save-fabricated-technical-review-0001".to_owned(),
        })
        .expect_err("fabricated technical review must fail");
    assert!(matches!(fabricated, KernelError::ProfileSemantic(_)));

    let mut accepted = rebase.draft.snapshot.clone();
    let accepted_field = accepted
        .entities
        .iter_mut()
        .find(|entity| entity.entity_id == ROOT_ENTITY)
        .and_then(|entity| {
            entity
                .fields
                .iter_mut()
                .find(|field| field.field_id == "purchaseOrderByCustomer")
        })
        .expect("changed field to accept");
    accepted_field.state = DestinationFieldState::Active;
    accepted_field.technical_change_review = Some(TechnicalChangeReviewEvidence {
        decision: TechnicalChangeDecision::AcceptRevisedTechnicalEvidence,
        actor: "Destination practitioner".to_owned(),
        reason: "Accept the exact revised maximum while retaining reviewed business meaning."
            .to_owned(),
    });
    let accepted = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: rebase.draft.draft_id.clone(),
            expected_draft_generation: rebase.draft.generation,
            expected_draft_hash: rebase.draft.draft_hash.clone(),
            snapshot: accepted,
            actor: "Destination practitioner".to_owned(),
            reason: "Accept the exact changed technical evidence.".to_owned(),
            idempotency_key: "destination-save-accepted-technical-review-0001".to_owned(),
        })
        .expect("save exact technical review");
    assert_eq!(accepted.freeze_blockers, [] as [std::string::String; 0]);
    let accepted_frozen = service
        .destination_service()
        .freeze_draft(&FreezeDestinationDraftCommand {
            schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: accepted.draft_id.clone(),
            expected_draft_generation: accepted.generation,
            expected_draft_hash: accepted.draft_hash.clone(),
            profile_version: "1.1.0".to_owned(),
            acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
            actor: "Destination reviewer".to_owned(),
            reason: "Freeze the exact reviewed technical revision.".to_owned(),
            idempotency_key: "destination-freeze-accepted-technical-review-0001".to_owned(),
        })
        .expect("freeze accepted technical change");

    let next = changed.replace(
        "Name=\"DistributionChannel\" Type=\"Edm.String\" Nullable=\"true\" MaxLength=\"2\"",
        "Name=\"DistributionChannel\" Type=\"Edm.String\" Nullable=\"true\" MaxLength=\"3\"",
    );
    assert_ne!(next, changed);
    let next_artifact =
        import_destination_bytes(&service, &workspace, next.as_bytes(), "technical-next");
    let next_rebase = service
        .destination_service()
        .rebase_draft(&RebaseDestinationDraftCommand {
            schema_version: REBASE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: accepted_frozen.draft_id,
            expected_draft_generation: accepted_frozen.draft_generation,
            expected_draft_hash: accepted_frozen.draft_hash,
            target_catalogue_authority_id: next_artifact.catalogue_authority_id,
            expected_target_catalogue_hash: next_artifact.catalogue_hash,
            actor: "Destination practitioner".to_owned(),
            reason: "Rebase again and clear superseded technical review evidence.".to_owned(),
            idempotency_key: "destination-rebase-clear-technical-review-0001".to_owned(),
        })
        .expect("rebase accepted field onto unchanged technical evidence");
    let cleared_field = next_rebase
        .draft
        .snapshot
        .entities
        .iter()
        .find(|entity| entity.entity_id == ROOT_ENTITY)
        .and_then(|entity| {
            entity
                .fields
                .iter()
                .find(|field| field.field_id == "purchaseOrderByCustomer")
        })
        .expect("rebased reviewed field");
    assert_eq!(
        cleared_field.diff_state,
        Some(DestinationDiffState::Unchanged)
    );
    assert_eq!(cleared_field.state, DestinationFieldState::Active);
    assert!(cleared_field.technical_change_review.is_none());
    let integrity = integrity_check(temporary.path()).expect("technical-change integrity replay");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn generated_entity_and_field_ids_are_collision_safe_and_rebase_deterministic() {
    let temporary = TempDir::new().expect("canonical-ID collision workspace");
    let (service, _) = create_workspace_and_engagement(temporary.path());
    let root_properties = vec![
        ManualPropertyDefinition {
            name: "Id".to_owned(),
            edm_type: "Edm.String".to_owned(),
            nullable: false,
            max_length: Some(20),
            key_candidate: true,
        },
        ManualPropertyDefinition {
            name: "Initial-Code".to_owned(),
            edm_type: "Edm.String".to_owned(),
            nullable: false,
            max_length: Some(20),
            key_candidate: false,
        },
        ManualPropertyDefinition {
            name: "Initial_Code".to_owned(),
            edm_type: "Edm.String".to_owned(),
            nullable: false,
            max_length: Some(20),
            key_candidate: false,
        },
        ManualPropertyDefinition {
            name: "Future-Code".to_owned(),
            edm_type: "Edm.String".to_owned(),
            nullable: false,
            max_length: Some(20),
            key_candidate: false,
        },
    ];
    let child = ManualEntityDefinition {
        namespace: "Beta".to_owned(),
        name: "Record".to_owned(),
        properties: vec![ManualPropertyDefinition {
            name: "Id".to_owned(),
            edm_type: "Edm.String".to_owned(),
            nullable: false,
            max_length: Some(20),
            key_candidate: true,
        }],
    };
    let original = service
        .destination_service()
        .create_manual_catalogue(&CreateManualCatalogueCommand {
            schema_version: CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            artifact_label: "Colliding canonical-name evidence".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Prove deterministic suggestions without changing technical identities."
                .to_owned(),
            entities: vec![
                ManualEntityDefinition {
                    namespace: "Alpha".to_owned(),
                    name: "Record".to_owned(),
                    properties: root_properties.clone(),
                },
                child.clone(),
            ],
            idempotency_key: "destination-create-collision-catalogue-0001".to_owned(),
        })
        .expect("create collision catalogue");
    let draft = service
        .destination_service()
        .start_draft(&StartDestinationDraftCommand {
            schema_version: START_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            catalogue_authority_id: original.catalogue_authority_id.clone(),
            expected_catalogue_hash: original.catalogue_hash.clone(),
            root_technical_entity: "Alpha.Record".to_owned(),
            child_technical_entities: vec!["Beta.Record".to_owned()],
            profile_id: "synthetic.workspace.collision-proof".to_owned(),
            title: "Collision-safe destination suggestions".to_owned(),
            owner: "Destination practitioner".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Select two same-named entities from different namespaces.".to_owned(),
            idempotency_key: "destination-start-collision-draft-0001".to_owned(),
        })
        .expect("start collision-safe draft");
    let entity_ids = draft
        .snapshot
        .entities
        .iter()
        .map(|entity| entity.entity_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(entity_ids.len(), 2);
    assert!(
        draft
            .snapshot
            .entities
            .iter()
            .all(|entity| entity.entity_id.starts_with("record"))
    );
    let root = draft
        .snapshot
        .entities
        .iter()
        .find(|entity| entity.technical_entity.as_deref() == Some("Alpha.Record"))
        .expect("root collision entity");
    let field_ids = root
        .fields
        .iter()
        .map(|field| field.field_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(field_ids.len(), root.fields.len());
    let initial_hyphen = root
        .fields
        .iter()
        .find(|field| field.technical_property.as_deref() == Some("Alpha.Record.Initial-Code"))
        .expect("hyphen field");
    let initial_underscore = root
        .fields
        .iter()
        .find(|field| field.technical_property.as_deref() == Some("Alpha.Record.Initial_Code"))
        .expect("underscore field");
    assert_ne!(initial_hyphen.field_id, initial_underscore.field_id);

    let mut operator_edited = draft.snapshot.clone();
    operator_edited
        .entities
        .iter_mut()
        .find(|entity| entity.technical_entity.as_deref() == Some("Alpha.Record"))
        .and_then(|entity| {
            entity
                .fields
                .iter_mut()
                .find(|field| field.technical_property.as_deref() == Some("Alpha.Record.Id"))
        })
        .expect("operator-editable ID")
        .field_id = "operatorId".to_owned();
    let saved = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id.clone(),
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash.clone(),
            snapshot: operator_edited,
            actor: "Destination practitioner".to_owned(),
            reason: "Exercise the operator-editable canonical identity.".to_owned(),
            idempotency_key: "destination-save-operator-canonical-id-0001".to_owned(),
        })
        .expect("save operator-edited canonical ID");

    let mut revised_root_properties = root_properties;
    revised_root_properties.push(ManualPropertyDefinition {
        name: "Future_Code".to_owned(),
        edm_type: "Edm.String".to_owned(),
        nullable: false,
        max_length: Some(20),
        key_candidate: false,
    });
    let revised = service
        .destination_service()
        .create_manual_catalogue(&CreateManualCatalogueCommand {
            schema_version: CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            artifact_label: "Revised colliding canonical-name evidence".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Add one required candidate whose readable canonical ID collides.".to_owned(),
            entities: vec![
                ManualEntityDefinition {
                    namespace: "Alpha".to_owned(),
                    name: "Record".to_owned(),
                    properties: revised_root_properties,
                },
                child,
            ],
            idempotency_key: "destination-create-revised-collision-catalogue-0001".to_owned(),
        })
        .expect("create revised collision catalogue");
    let command = RebaseDestinationDraftCommand {
        schema_version: REBASE_DRAFT_COMMAND_SCHEMA.to_owned(),
        expected_engagement_generation: service.state().expect("state").generation,
        draft_id: saved.draft_id.clone(),
        expected_draft_generation: saved.generation,
        expected_draft_hash: saved.draft_hash.clone(),
        target_catalogue_authority_id: revised.catalogue_authority_id,
        expected_target_catalogue_hash: revised.catalogue_hash,
        actor: "Destination practitioner".to_owned(),
        reason: "Rebase while retaining exact operator IDs and allocating one collision."
            .to_owned(),
        idempotency_key: "destination-rebase-canonical-collision-0001".to_owned(),
    };
    let rebased = service
        .destination_service()
        .rebase_draft(&command)
        .expect("rebase collision-safe draft");
    assert_eq!(
        service
            .destination_service()
            .rebase_draft(&command)
            .expect("exact deterministic rebase retry"),
        rebased
    );
    let root = rebased
        .draft
        .snapshot
        .entities
        .iter()
        .find(|entity| entity.technical_entity.as_deref() == Some("Alpha.Record"))
        .expect("rebased root");
    assert!(
        root.fields.iter().any(|field| {
            field.technical_property.as_deref() == Some("Alpha.Record.Id")
                && field.field_id == "operatorId"
        }),
        "rebase must preserve the operator-edited canonical ID"
    );
    let prior = root
        .fields
        .iter()
        .find(|field| field.technical_property.as_deref() == Some("Alpha.Record.Future-Code"))
        .expect("existing colliding field");
    let added = root
        .fields
        .iter()
        .find(|field| field.technical_property.as_deref() == Some("Alpha.Record.Future_Code"))
        .expect("rebase-added colliding field");
    assert_eq!(prior.field_id, "future_Code");
    assert_ne!(added.field_id, prior.field_id);
    assert!(added.field_id.starts_with("future_Code_"));
    assert_eq!(added.diff_state, Some(DestinationDiffState::Added));
    let unique_rebased_ids = root
        .fields
        .iter()
        .map(|field| field.field_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(unique_rebased_ids.len(), root.fields.len());
    drop(service);
    let service = reopen_engagement(temporary.path());
    assert_eq!(
        service
            .destination_service()
            .workshop()
            .expect("reopen collision-safe workshop")
            .draft,
        Some(rebased.draft)
    );
    let integrity = integrity_check(temporary.path()).expect("collision integrity replay");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn destination_profile_stage_and_draft_head_tamper_fail_closed() {
    let approval_temporary = TempDir::new().expect("profile-stage guard workspace");
    let (service, workspace) = create_workspace_and_engagement(approval_temporary.path());
    let artifact = import_edmx(
        &service,
        &workspace,
        "fixtures/metadata/sanitized-sales-v4.edmx",
        "approval-stage-guard",
    );
    let draft = start_single_entity_draft(
        &service,
        &artifact,
        "Synthetic.ODataV4.Sales.Attachment",
        "synthetic.workspace.approval-stage-guard",
        "approval-stage-guard",
    );
    let saved = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id.clone(),
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash.clone(),
            snapshot: confirm_all_suggested_fields(&draft.snapshot),
            actor: "Destination practitioner".to_owned(),
            reason: "Prepare an exact unapproved reviewed profile.".to_owned(),
            idempotency_key: "destination-save-approval-stage-guard-0001".to_owned(),
        })
        .expect("save approval-stage guard draft");
    service
        .destination_service()
        .freeze_draft(&FreezeDestinationDraftCommand {
            schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: saved.draft_id.clone(),
            expected_draft_generation: saved.generation,
            expected_draft_hash: saved.draft_hash.clone(),
            profile_version: "1.0.0".to_owned(),
            acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
            actor: "Destination reviewer".to_owned(),
            reason: "Freeze without granting mapping authority.".to_owned(),
            idempotency_key: "destination-freeze-approval-stage-guard-0001".to_owned(),
        })
        .expect("freeze unapproved profile");
    let connection = Connection::open(approval_temporary.path().join("state.sqlite3"))
        .expect("open profile-stage authority database");
    assert!(
        connection
            .execute(
                "UPDATE engagement_active_context SET profile_stage = 'APPROVED'",
                [],
            )
            .is_err(),
        "database guard must reject APPROVED without exact registry approval"
    );
    connection
        .execute_batch(
            "DROP TRIGGER engagement_active_destination_update_guard;
             UPDATE engagement_active_context SET profile_stage = 'APPROVED';",
        )
        .expect("simulate coherent stage tamper without approval");
    drop(connection);
    assert!(service.destination_service().workshop().is_err());
    assert!(service.mapping_workspace().is_err());
    assert!(service.journey().is_err());
    let tampered = integrity_check(approval_temporary.path()).expect("profile-stage tamper replay");
    assert!(!tampered.ok);
    assert!(tampered.issues.iter().any(|issue| {
        issue.contains("approval authority")
            || issue.contains("active approved workspace destination")
    }));

    let head_temporary = TempDir::new().expect("draft-head guard workspace");
    let (service, workspace) = create_workspace_and_engagement(head_temporary.path());
    let artifact = import_edmx(
        &service,
        &workspace,
        "fixtures/metadata/sanitized-sales-v4.edmx",
        "draft-head-guard",
    );
    let draft = start_single_entity_draft(
        &service,
        &artifact,
        "Synthetic.ODataV4.Sales.Attachment",
        "synthetic.workspace.draft-head-guard",
        "draft-head-guard",
    );
    let saved = service
        .destination_service()
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id.clone(),
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash.clone(),
            snapshot: confirm_all_suggested_fields(&draft.snapshot),
            actor: "Destination practitioner".to_owned(),
            reason: "Create a second immutable destination revision.".to_owned(),
            idempotency_key: "destination-save-draft-head-guard-0001".to_owned(),
        })
        .expect("save second draft revision");
    let connection = Connection::open(head_temporary.path().join("state.sqlite3"))
        .expect("open draft-head authority database");
    assert!(
        connection
            .execute(
                "UPDATE destination_profile_draft_heads
                 SET generation = 1, head_revision_hash = ?1
                 WHERE draft_id = ?2",
                rusqlite::params![draft.draft_hash, draft.draft_id],
            )
            .is_err(),
        "destination head guard must reject rollback to an earlier valid revision"
    );
    connection
        .execute(
            "INSERT INTO destination_profile_draft_revisions (
                 draft_id, generation, revision_hash, parent_revision_hash,
                 canonical_draft_json, actor, reason, created_at
             ) VALUES (?1, 3, ?2, ?3, '{}', 'tamper', 'orphan', '2026-01-01T00:00:00Z')",
            rusqlite::params![saved.draft_id, "2".repeat(64), saved.draft_hash],
        )
        .expect("simulate an orphan next revision without advancing the guarded head");
    drop(connection);
    assert!(service.destination_service().workshop().is_err());
    let orphaned = integrity_check(head_temporary.path()).expect("orphan revision replay");
    assert!(!orphaned.ok);
    assert!(
        orphaned.issues.iter().any(|issue| {
            issue.contains("revision chain") || issue.contains("destination draft")
        })
    );
}

#[test]
fn compatibility_report_is_rebuilt_from_exact_immutable_edmx() {
    let temporary = TempDir::new().expect("compatibility replay workspace");
    let (service, workspace) = create_workspace_and_engagement(temporary.path());
    let artifact = import_edmx(
        &service,
        &workspace,
        "fixtures/metadata/sanitized-sales-v2.edmx",
        "report-replay",
    );
    let connection =
        Connection::open(temporary.path().join("state.sqlite3")).expect("open authority database");
    let stored: String = connection
        .query_row(
            "SELECT compatibility_report_json
             FROM destination_catalogue_authorities
             WHERE catalogue_authority_id = ?1",
            [&artifact.catalogue_authority_id],
            |row| row.get(0),
        )
        .expect("load compatibility report");
    let mut report: serde_json::Value = serde_json::from_str(&stored).expect("parse report");
    let changed_count = report["safelyExtractedCount"]
        .as_u64()
        .expect("safe extracted count")
        .saturating_add(1);
    report["safelyExtractedCount"] = json!(changed_count);
    let basis = json!({
        "schemaVersion": "migration-stream/destination-compatibility/v1",
        "algorithm": report["algorithm"],
        "odataGeneration": report["odataGeneration"],
        "safelyExtractedCount": report["safelyExtractedCount"],
        "unsupportedInventoryCount": report["unsupportedInventoryCount"],
        "unsupportedIrrelevantCount": report["unsupportedIrrelevantCount"],
        "unresolvedSelectedScopeCount": report["unresolvedSelectedScopeCount"],
        "findings": report["findings"],
    });
    report["reportHash"] = json!(canonical_hash(&basis).expect("tampered inner report hash"));
    let tampered_json = canonical_json_string(&report).expect("canonical tampered report");
    let tampered_hash = canonical_hash(&report).expect("tampered outer report hash");
    connection
        .execute_batch("DROP TRIGGER destination_catalogue_authorities_no_update;")
        .expect("remove immutability guard for tamper simulation");
    connection
        .execute(
            "UPDATE destination_catalogue_authorities
             SET compatibility_report_json = ?1, compatibility_report_hash = ?2
             WHERE catalogue_authority_id = ?3",
            rusqlite::params![
                tampered_json,
                tampered_hash,
                artifact.catalogue_authority_id
            ],
        )
        .expect("simulate coherently rehashed compatibility report");
    drop(connection);
    assert!(service.destination_service().workshop().is_err());
    let replay = integrity_check(temporary.path()).expect("compatibility tamper replay");
    assert!(!replay.ok);
    assert!(replay.issues.iter().any(|issue| {
        issue.contains("compatibility report does not recompute") || issue.contains("compatibility")
    }));
}

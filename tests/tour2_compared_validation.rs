use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::HOST},
};
use migration_stream::{
    KernelError,
    canonical::{canonical_hash, canonical_json_string},
    engagement::{
        ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA, ACCEPT_SOURCE_COMMAND_SCHEMA,
        APPROVE_MAPPING_VERSION_COMMAND_SCHEMA, APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA,
        AcceptPreflightCommand, AcceptRevisedPreflightCommand, ApproveMappingVersionCommand,
        ApproveProfileCommand, CREATE_ENGAGEMENT_COMMAND_SCHEMA, CompletePreflightCommand,
        ControlledObjectPack, CreateOrReopenCommand, EngagementService, EngagementServiceConfig,
        LAUNCH_REVISED_VALIDATION_COMMAND_SCHEMA, LAUNCH_VALIDATION_COMMAND_SCHEMA,
        LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT, LaunchRevisedValidationCommand,
        LaunchRevisedValidationResult, LaunchValidationCommand, MappingDraftSnapshot, MappingMode,
        OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT, OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT,
        ObjectPackMappingSuggestion, RESERVE_PREFLIGHT_COMMAND_SCHEMA,
        RESERVE_REVISED_PREFLIGHT_COMMAND_SCHEMA, REVISED_SOURCE_ACKNOWLEDGEMENT,
        ReservePreflightCommand, ReserveRevisedPreflightCommand, SAVE_MAPPING_DRAFT_COMMAND_SCHEMA,
        SELECT_OBJECT_PACK_COMMAND_SCHEMA, SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT,
        START_REVISED_TOUR_COMMAND_SCHEMA, SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA,
        SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID, SaveMappingDraftCommand, SelectObjectPackCommand,
        StartRevisedTourCommand, SubmitMappingDraftCommand,
    },
    journey::{JourneyStageId, JourneyStageStatus, MULTI_ENTITY_DOWNSTREAM_REASON},
    mapping::{
        MappingBinding, MappingKind, MappingRelationshipBinding, ReferenceTable,
        RelationshipFieldPair, SourceEntityAssignment,
    },
    policy::IntakePolicy,
    profiling::ProfilingPolicy,
    state::integrity_check,
    validation::ValidationPolicy,
    workbench::{WorkbenchConfig, build_router},
};
use rusqlite::{Connection, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tower::ServiceExt as _;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const HEADER_ENTITY: &str = "SalesDocument";
const ITEM_ENTITY: &str = "SalesDocumentItem";

struct CompletedTourOne {
    temporary: TempDir,
    service: EngagementService,
    validation_run_id: String,
}

struct PreparedTourTwo {
    tour_attempt_id: String,
    generation: u64,
    reuse_authority_id: String,
    reuse_authority_hash: String,
    baseline_source_hash: String,
    mapping_origin_source_hash: String,
    profiling_run_id: String,
}

fn repository_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn sales_reference_tables() -> Vec<ReferenceTable> {
    vec![
        ReferenceTable {
            table_id: "sales-document-type-domain".to_owned(),
            version: "1.0.0".to_owned(),
            entity_id: Some(HEADER_ENTITY.to_owned()),
            values: BTreeMap::from([
                ("RETURN".to_owned(), "RETURN".to_owned()),
                ("STANDARD".to_owned(), "STANDARD".to_owned()),
            ]),
        },
        ReferenceTable {
            table_id: "sales-document-route-map".to_owned(),
            version: "1.0.0".to_owned(),
            entity_id: Some(HEADER_ENTITY.to_owned()),
            values: BTreeMap::from([
                ("N".to_owned(), "STANDARD".to_owned()),
                ("R".to_owned(), "RETURN".to_owned()),
            ]),
        },
    ]
}

fn sales_pack() -> ControlledObjectPack {
    ControlledObjectPack {
        object_pack_id: SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID.to_owned(),
        version: "1.0.0".to_owned(),
        title: "Synthetic basic sales document readiness pack".to_owned(),
        description: "Synthetic target-neutral two-file readiness proof.".to_owned(),
        scope: "Synthetic target-neutral two-file sales-document readiness validation proof only."
            .to_owned(),
        root_entity_id: HEADER_ENTITY.to_owned(),
        mapping_mode: MappingMode::OperatorGoverned,
        limitations: vec![
            "No target-system compatibility, publication authority, or load compatibility claim.".to_owned(),
            "Multi-entity authority stops at read-only First Issues.".to_owned(),
        ],
        mapping_suggestions: Vec::new(),
        reference_tables: sales_reference_tables(),
        profile_document: serde_json::from_slice(
            &fs::read(repository_path(
                "object-packs/synthetic.sales-document.readiness/1.0.0/synthetic-profile.reviewed.json",
            ))
            .expect("read sales-document profile"),
        )
        .expect("parse sales-document profile"),
    }
}

fn customer_pack() -> ControlledObjectPack {
    ControlledObjectPack {
        object_pack_id: "synthetic.customer.readiness".to_owned(),
        version: "1.0.0".to_owned(),
        title: "Synthetic customer readiness".to_owned(),
        description: "Synthetic local single-entity readiness proof.".to_owned(),
        scope: "Synthetic local proof only.".to_owned(),
        root_entity_id: "Customer".to_owned(),
        mapping_mode: MappingMode::ControlledSuggestions,
        limitations: vec!["No target compatibility claim.".to_owned()],
        mapping_suggestions: vec![ObjectPackMappingSuggestion {
            suggestion_id: "customer-source-id".to_owned(),
            target_entity_id: Some("Customer".to_owned()),
            target_field_id: "sourceCustomerId".to_owned(),
            source_file: Some("customer_accounts.csv".to_owned()),
            source_column: Some("migrationPartnerId".to_owned()),
            source_column_ordinal: Some(0),
            kind: MappingKind::Direct,
            constant: None,
            lookup_table_id: None,
        }],
        reference_tables: Vec::new(),
        profile_document: serde_json::from_slice(
            &fs::read(repository_path(
                "object-packs/synthetic.customer.readiness/1.0.0/synthetic-profile.reviewed.json",
            ))
            .expect("read customer profile"),
        )
        .expect("parse customer profile"),
    }
}

fn sales_service(workspace: &Path) -> EngagementService {
    sales_service_with_packs(workspace, vec![sales_pack()])
}

fn sales_service_with_packs(
    workspace: &Path,
    object_packs: Vec<ControlledObjectPack>,
) -> EngagementService {
    let validation_policy =
        ValidationPolicy::from_path(&repository_path("config/engagement-validation-policy.json"))
            .expect("validation policy");
    sales_service_with_policy(workspace, object_packs, validation_policy)
}

fn sales_service_with_policy(
    workspace: &Path,
    object_packs: Vec<ControlledObjectPack>,
    validation_policy: ValidationPolicy,
) -> EngagementService {
    migration_stream::object_workspace::establish_for_local_automation(
        workspace,
        "Synthetic compared-validation workspace",
        "Basic sales document (header and items)",
        "Automated integration proof",
    )
    .expect("initialize object workspace");
    let mut profiling_policy =
        ProfilingPolicy::from_path(&repository_path("config/engagement-profiling-policy.json"))
            .expect("profiling policy");
    profiling_policy.maximum_samples_per_column = 5;
    profiling_policy.maximum_sample_value_bytes = 256;
    EngagementService::new(EngagementServiceConfig {
        workspace: workspace.to_path_buf(),
        intake_policy: IntakePolicy::from_path(Some(&repository_path(
            "config/engagement-intake-policy.json",
        )))
        .expect("intake policy"),
        profiling_policy,
        profile_schema_path: repository_path("schemas/migration-object-profile.schema.json"),
        mapping_schema_path: repository_path("schemas/source-mapping-version.schema.json"),
        validation_policy,
        object_packs,
    })
    .expect("engagement service")
}

fn build_zip(path: &Path, fixture_directory: &str, drift_header: bool) {
    build_zip_variant(path, fixture_directory, drift_header, false);
}

fn build_zip_variant(path: &Path, fixture_directory: &str, drift_header: bool, reorder_rows: bool) {
    let file = File::create(path).expect("create source zip");
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for source_file in ["deal_book.csv", "deal_detail.csv"] {
        writer
            .start_file(source_file, options)
            .expect("start source member");
        let mut bytes = fs::read(repository_path(&format!(
            "{fixture_directory}/{source_file}"
        )))
        .expect("read source fixture");
        if drift_header && source_file == "deal_book.csv" {
            let text = String::from_utf8(bytes).expect("UTF-8 sales fixture");
            bytes = text
                .replacen("owning_desk", "unexpected_desk", 1)
                .into_bytes();
        }
        if reorder_rows {
            let text = String::from_utf8(bytes).expect("UTF-8 sales fixture");
            let mut lines = text.lines();
            let header = lines.next().expect("fixture header");
            let mut rows = lines.collect::<Vec<_>>();
            rows.rotate_left(1);
            bytes = format!("{header}\n{}\n", rows.join("\n")).into_bytes();
        }
        writer.write_all(&bytes).expect("write source member");
    }
    writer.finish().expect("finish source zip");
}

fn file_identity(path: &Path) -> (String, u64) {
    let bytes = fs::read(path).expect("source package bytes");
    (
        format!("{:064x}", Sha256::digest(&bytes)),
        u64::try_from(bytes.len()).expect("source package size"),
    )
}

async fn direct_workbench_get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(HOST, "127.0.0.1:4173")
                .body(Body::empty())
                .expect("bounded direct-workbench request"),
        )
        .await
        .expect("infallible direct-workbench response");
    let status = response.status();
    let body = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("bounded direct-workbench body");
    let document = serde_json::from_slice(&body).expect("direct-workbench JSON");
    (status, document)
}

#[allow(clippy::too_many_arguments)]
fn binding(
    binding_id: &str,
    entity_id: &str,
    source_file: &str,
    source_column: &str,
    source_column_ordinal: usize,
    target_field_id: &str,
    kind: MappingKind,
    lookup_table_id: Option<&str>,
) -> MappingBinding {
    MappingBinding {
        binding_id: binding_id.to_owned(),
        target_entity_id: Some(entity_id.to_owned()),
        source_file: Some(source_file.to_owned()),
        source_column: Some(source_column.to_owned()),
        source_column_ordinal: Some(source_column_ordinal),
        target_field_id: target_field_id.to_owned(),
        kind,
        constant: None,
        lookup_table_id: lookup_table_id.map(str::to_owned),
    }
}

#[allow(clippy::too_many_lines)]
fn governed_snapshot() -> MappingDraftSnapshot {
    MappingDraftSnapshot {
        source_entity_assignments: vec![
            SourceEntityAssignment {
                source_file: "deal_book.csv".to_owned(),
                entity_id: HEADER_ENTITY.to_owned(),
            },
            SourceEntityAssignment {
                source_file: "deal_detail.csv".to_owned(),
                entity_id: ITEM_ENTITY.to_owned(),
            },
        ],
        bindings: vec![
            binding(
                "header.source-document-id",
                HEADER_ENTITY,
                "deal_book.csv",
                "case_ref",
                0,
                "sourceDocumentId",
                MappingKind::Direct,
                None,
            ),
            binding(
                "header.customer-id",
                HEADER_ENTITY,
                "deal_book.csv",
                "party_ref",
                1,
                "customerId",
                MappingKind::Trim,
                None,
            ),
            binding(
                "header.document-date",
                HEADER_ENTITY,
                "deal_book.csv",
                "booked_on",
                2,
                "documentDate",
                MappingKind::Direct,
                None,
            ),
            binding(
                "header.document-type",
                HEADER_ENTITY,
                "deal_book.csv",
                "route_code",
                3,
                "documentType",
                MappingKind::Lookup,
                Some("sales-document-route-map"),
            ),
            binding(
                "header.currency",
                HEADER_ENTITY,
                "deal_book.csv",
                "money_iso",
                4,
                "currency",
                MappingKind::UppercaseAscii,
                None,
            ),
            binding(
                "header.customer-reference",
                HEADER_ENTITY,
                "deal_book.csv",
                "buy_ref",
                5,
                "customerReference",
                MappingKind::EmptyAsNull,
                None,
            ),
            binding(
                "header.selling-unit",
                HEADER_ENTITY,
                "deal_book.csv",
                "owning_desk",
                6,
                "sellingUnit",
                MappingKind::UppercaseAscii,
                None,
            ),
            binding(
                "item.parent-document-id",
                ITEM_ENTITY,
                "deal_detail.csv",
                "case_link",
                0,
                "parentDocumentId",
                MappingKind::Direct,
                None,
            ),
            binding(
                "item.source-item-id",
                ITEM_ENTITY,
                "deal_detail.csv",
                "seq_ref",
                1,
                "sourceItemId",
                MappingKind::Direct,
                None,
            ),
            binding(
                "item.product-id",
                ITEM_ENTITY,
                "deal_detail.csv",
                "offer_ref",
                2,
                "productId",
                MappingKind::Trim,
                None,
            ),
            binding(
                "item.quantity",
                ITEM_ENTITY,
                "deal_detail.csv",
                "demand_qty",
                3,
                "quantity",
                MappingKind::Direct,
                None,
            ),
            binding(
                "item.unit-of-measure",
                ITEM_ENTITY,
                "deal_detail.csv",
                "measure",
                4,
                "unitOfMeasure",
                MappingKind::UppercaseAscii,
                None,
            ),
            binding(
                "item.unit-price",
                ITEM_ENTITY,
                "deal_detail.csv",
                "net_rate",
                5,
                "unitPrice",
                MappingKind::EmptyAsNull,
                None,
            ),
            binding(
                "item.requested-date",
                ITEM_ENTITY,
                "deal_detail.csv",
                "need_by",
                6,
                "requestedDate",
                MappingKind::EmptyAsNull,
                None,
            ),
        ],
        relationship_bindings: vec![MappingRelationshipBinding {
            relationship_id: "sales-document.items".to_owned(),
            from_entity_id: HEADER_ENTITY.to_owned(),
            to_entity_id: ITEM_ENTITY.to_owned(),
            cardinality: "1..*".to_owned(),
            key_pairs: vec![RelationshipFieldPair {
                from_field_id: "sourceDocumentId".to_owned(),
                to_field_id: "parentDocumentId".to_owned(),
            }],
        }],
        reference_tables: sales_reference_tables(),
        author: Some("tour-test-operator".to_owned()),
    }
}

#[allow(clippy::too_many_lines)]
fn complete_tour_one() -> CompletedTourOne {
    let temporary = TempDir::new().expect("temporary tour workspace");
    let service = sales_service(temporary.path());
    service
        .create_or_reopen(&CreateOrReopenCommand {
            schema_version: CREATE_ENGAGEMENT_COMMAND_SCHEMA.to_owned(),
            display_name: "Compared sales validation".to_owned(),
            idempotency_key: "tour-create-command".to_owned(),
        })
        .expect("create engagement");

    let reservation = service
        .reserve_preflight(&ReservePreflightCommand {
            schema_version: RESERVE_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            source_name: "sales-tour-1.zip".to_owned(),
            idempotency_key: "tour-1-reserve-source".to_owned(),
        })
        .expect("reserve Tour 1 source");
    let source_zip = temporary.path().join("tour-1.zip");
    build_zip(&source_zip, "fixtures/sales-document-unfamiliar", false);
    fs::copy(&source_zip, &reservation.controlled_path).expect("stream Tour 1 source");
    let (source_hash, source_size) = file_identity(&source_zip);
    let preflight = service
        .complete_preflight(&CompletePreflightCommand {
            preflight_id: reservation.preflight.preflight_id,
            sha256: source_hash.clone(),
            size_bytes: source_size,
        })
        .expect("complete Tour 1 preflight");
    let accepted = service
        .accept_preflight(&AcceptPreflightCommand {
            schema_version: ACCEPT_SOURCE_COMMAND_SCHEMA.to_owned(),
            preflight_id: preflight.preflight_id,
            expected_source_hash: source_hash,
            expected_manifest_hash: preflight.manifest_hash.expect("Tour 1 manifest hash"),
            idempotency_key: "tour-1-accept-source".to_owned(),
            acknowledgement: SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("accept Tour 1");
    service
        .select_object_pack(&SelectObjectPackCommand {
            schema_version: SELECT_OBJECT_PACK_COMMAND_SCHEMA.to_owned(),
            object_pack_id: SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID.to_owned(),
            idempotency_key: "tour-1-select-pack".to_owned(),
        })
        .expect("select sales pack");
    service
        .approve_profile(&ApproveProfileCommand {
            schema_version: APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA.to_owned(),
            actor: "tour-test-operator".to_owned(),
            reason: "Approve the exact reviewed synthetic profile for Tour 1.".to_owned(),
            authority_acknowledgement: LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT
                .to_owned(),
            idempotency_key: "tour-1-approve-profile".to_owned(),
        })
        .expect("approve profile");
    assert_eq!(
        service.journey().expect("operator mapping journey").stages[3]
            .href
            .as_deref(),
        Some("/mapping")
    );
    let workspace = service.mapping_workspace().expect("mapping workspace");
    let saved = service
        .save_mapping_draft(&SaveMappingDraftCommand {
            schema_version: SAVE_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "tour-1-save-mapping".to_owned(),
            expected_engagement_generation: workspace.engagement_generation,
            expected_draft_id: None,
            expected_draft_generation: None,
            expected_draft_hash: None,
            snapshot: governed_snapshot(),
        })
        .expect("save mapping");
    let submitted = service
        .submit_mapping_draft(&SubmitMappingDraftCommand {
            schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "tour-1-submit-mapping".to_owned(),
            expected_engagement_generation: saved.engagement_generation,
            draft_id: saved.draft.draft_id,
            expected_draft_generation: saved.draft.draft_generation,
            expected_draft_hash: saved.draft.draft_hash,
            mapping_id: "synthetic.sales-document.tour-comparison".to_owned(),
            version: "1.0.0".to_owned(),
            acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("submit mapping");
    let submitted_workspace = service
        .mapping_workspace()
        .expect("submitted mapping workspace");
    let submitted_draft = submitted_workspace.draft.expect("submitted draft");
    let approval = service
        .approve_mapping_version(&ApproveMappingVersionCommand {
            schema_version: APPROVE_MAPPING_VERSION_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "tour-1-approve-mapping".to_owned(),
            expected_engagement_generation: submitted_workspace.engagement_generation,
            draft_id: submitted_draft.draft_id,
            expected_draft_generation: submitted_draft.draft_generation,
            expected_draft_hash: submitted_draft.draft_hash,
            expected_mapping_id: "synthetic.sales-document.tour-comparison".to_owned(),
            expected_mapping_version: "1.0.0".to_owned(),
            expected_mapping_hash: submitted.mapping_hash,
            actor: "tour-test-operator".to_owned(),
            reason: "Approve the exact immutable mapping for local compared validation.".to_owned(),
            acknowledgement: OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("approve mapping");
    let validation = service
        .launch_validation(&LaunchValidationCommand {
            schema_version: LAUNCH_VALIDATION_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "tour-1-launch-validation".to_owned(),
            expected_engagement_generation: service.state().expect("validation state").generation,
            expected_job_id: accepted.job_id,
            expected_mapping_id: approval.mapping_id,
            expected_mapping_version: approval.mapping_version,
            expected_mapping_hash: approval.mapping_hash,
        })
        .expect("validate Tour 1");
    assert_eq!(validation.status.rows_processed, 10);
    assert_eq!(validation.status.issue_count, 5);
    CompletedTourOne {
        temporary,
        service,
        validation_run_id: validation.status.run_id,
    }
}

#[tokio::test]
async fn direct_workbench_cannot_enable_multi_entity_downstream_authority() {
    let fixture = complete_tour_one();
    let state = fixture.service.state().expect("Tour 1 state");
    let app = build_router(
        WorkbenchConfig::new(
            fixture.temporary.path(),
            state.active.job_id.expect("active Tour 1 job"),
            state.active.profiling_run_id,
        )
        .with_readiness_publication(),
    )
    .expect("bounded multi-entity workbench");

    let (context_status, context) = direct_workbench_get(&app, "/api/v1/context").await;
    assert_eq!(context_status, StatusCode::OK);
    assert_eq!(
        context["schema_version"],
        "migration-stream/workbench/context/v1"
    );
    assert_eq!(context["read_only"], true);
    assert!(context.get("capabilities").is_none());

    let (journey_status, journey) = direct_workbench_get(&app, "/api/v1/journey").await;
    assert_eq!(journey_status, StatusCode::OK);
    assert_eq!(journey["binding"]["entityCount"], 2);
    assert_eq!(journey["currentStageId"], "ISSUES");
    for stage in journey["stages"]
        .as_array()
        .expect("journey stages")
        .iter()
        .skip(7)
    {
        assert_eq!(stage["status"], "LOCKED");
        assert_eq!(stage["href"], Value::Null);
        assert_eq!(stage["unavailableReason"], MULTI_ENTITY_DOWNSTREAM_REASON);
    }

    for uri in [
        "/api/v1/decision-queue?page_size=1",
        "/api/v1/reconciliation?page_size=1",
        "/api/v1/result-candidates?page_size=1",
        "/api/v1/publications/eligibility",
    ] {
        let (status, _) = direct_workbench_get(&app, uri).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "multi-entity authority escaped through {uri}"
        );
    }
}

fn start_tour_two(fixture: &CompletedTourOne, prefix: &str) -> (String, u64, String) {
    let state = fixture.service.state().expect("Tour 1 state");
    let before = fixture
        .service
        .tour_chain()
        .expect("tour chain before start");
    let expected_generation = if before.active_tour.is_some() {
        before.engagement_generation
    } else {
        state.generation
    };
    let command = StartRevisedTourCommand {
        schema_version: START_REVISED_TOUR_COMMAND_SCHEMA.to_owned(),
        idempotency_key: format!("{prefix}-start"),
        expected_engagement_generation: expected_generation,
        actor: "tour-test-operator".to_owned(),
        reason: "Start an exact revised-source comparison against immutable Tour 1.".to_owned(),
        acknowledgement: REVISED_SOURCE_ACKNOWLEDGEMENT.to_owned(),
    };
    let chain = fixture
        .service
        .start_revised_tour(&command)
        .expect("start Tour 2");
    let replay = fixture
        .service
        .start_revised_tour(&command)
        .expect("replay Tour 2 start");
    assert_eq!(replay, chain);
    let attempt = chain.pending_attempt.expect("pending Tour 2");
    (
        attempt.tour_attempt_id,
        chain.engagement_generation,
        chain.active_tour.expect("active Tour 1").tour_id,
    )
}

fn prepare_tour_two(
    fixture: &CompletedTourOne,
    prefix: &str,
    drift_header: bool,
) -> Result<PreparedTourTwo, KernelError> {
    prepare_revision(
        fixture,
        prefix,
        "fixtures/sales-document-unfamiliar-tour-2",
        drift_header,
        false,
    )
}

fn prepare_revision(
    fixture: &CompletedTourOne,
    prefix: &str,
    fixture_directory: &str,
    drift_header: bool,
    reorder_rows: bool,
) -> Result<PreparedTourTwo, KernelError> {
    let (tour_attempt_id, start_generation, _) = start_tour_two(fixture, prefix);
    let reservation_command = ReserveRevisedPreflightCommand {
        schema_version: RESERVE_REVISED_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
        idempotency_key: format!("{prefix}-reserve"),
        expected_engagement_generation: start_generation,
        tour_attempt_id: tour_attempt_id.clone(),
        source_name: format!("{prefix}.zip"),
    };
    let reservation = fixture
        .service
        .reserve_revised_preflight(&reservation_command)?;
    let replayed_reservation = fixture
        .service
        .reserve_revised_preflight(&reservation_command)?;
    assert_eq!(
        replayed_reservation.preflight.preflight_id,
        reservation.preflight.preflight_id
    );
    let after_reserve = fixture.service.tour_chain().expect("reserved tour chain");

    let zip_path = fixture.temporary.path().join(format!("{prefix}.zip"));
    build_zip_variant(&zip_path, fixture_directory, drift_header, reorder_rows);
    fs::copy(&zip_path, &reservation.controlled_path).expect("stream revised source");
    let (source_hash, source_size) = file_identity(&zip_path);
    let preflight = fixture
        .service
        .complete_preflight(&CompletePreflightCommand {
            preflight_id: reservation.preflight.preflight_id,
            sha256: source_hash.clone(),
            size_bytes: source_size,
        })
        .expect("complete revised preflight");
    let accept_command = AcceptRevisedPreflightCommand {
        schema_version: ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA.to_owned(),
        idempotency_key: format!("{prefix}-accept"),
        expected_engagement_generation: after_reserve.engagement_generation,
        tour_attempt_id: tour_attempt_id.clone(),
        preflight_id: preflight.preflight_id,
        expected_source_hash: source_hash,
        expected_manifest_hash: preflight
            .manifest_hash
            .expect("revised source manifest hash"),
        actor: "tour-test-operator".to_owned(),
        reason: "Accept this exact synthetic revised source for compared validation.".to_owned(),
        acknowledgement: REVISED_SOURCE_ACKNOWLEDGEMENT.to_owned(),
    };
    let accepted = fixture.service.accept_revised_preflight(&accept_command)?;
    let replayed = fixture.service.accept_revised_preflight(&accept_command)?;
    assert!(replayed.idempotent_replay);
    assert_eq!(replayed.reuse_authority, accepted.reuse_authority);
    Ok(PreparedTourTwo {
        tour_attempt_id,
        generation: accepted.engagement_generation,
        reuse_authority_id: accepted.reuse_authority.reuse_authority_id,
        reuse_authority_hash: accepted.reuse_authority.reuse_authority_hash,
        baseline_source_hash: accepted.reuse_authority.baseline_source_hash,
        mapping_origin_source_hash: accepted.reuse_authority.mapping_origin_source_hash,
        profiling_run_id: accepted
            .tour_attempt
            .profiling_run_id
            .expect("profiling run"),
    })
}

fn launch_command(prepared: &PreparedTourTwo, prefix: &str) -> LaunchRevisedValidationCommand {
    LaunchRevisedValidationCommand {
        schema_version: LAUNCH_REVISED_VALIDATION_COMMAND_SCHEMA.to_owned(),
        idempotency_key: format!("{prefix}-launch"),
        expected_engagement_generation: prepared.generation,
        tour_attempt_id: prepared.tour_attempt_id.clone(),
        expected_reuse_authority_id: prepared.reuse_authority_id.clone(),
        expected_reuse_authority_hash: prepared.reuse_authority_hash.clone(),
        actor: "tour-test-operator".to_owned(),
        reason: "Validate and compare the exact revised source under replayed authority."
            .to_owned(),
        acknowledgement: REVISED_SOURCE_ACKNOWLEDGEMENT.to_owned(),
    }
}

fn complete_tour_two(fixture: &CompletedTourOne, prefix: &str) -> LaunchRevisedValidationResult {
    let prepared = prepare_tour_two(fixture, prefix, false).expect("prepare compatible Tour 2");
    fixture
        .service
        .launch_revised_validation(&launch_command(&prepared, prefix))
        .expect("complete compared Tour 2")
}

fn count(connection: &Connection, sql: &str) -> i64 {
    connection
        .query_row(sql, [], |row| row.get(0))
        .expect("count query")
}

fn delete_tour_commands(connection: &Connection, sql: &str) -> usize {
    connection
        .execute_batch("DROP TRIGGER engagement_tour_command_attempts_no_delete;")
        .expect("open command deletion window");
    let deleted = connection.execute(sql, []).expect("delete tour commands");
    connection
        .execute_batch(
            "CREATE TRIGGER engagement_tour_command_attempts_no_delete
                 BEFORE DELETE ON engagement_tour_command_attempts
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'engagement tour command attempts are immutable'
                     );
                 END;",
        )
        .expect("restore command deletion guard");
    deleted
}

#[allow(clippy::too_many_lines)]
fn coherently_shift_tour_command_generations(connection: &Connection, shift: i64) {
    let commands = connection
        .prepare(
            "SELECT attempt_id, command_kind, canonical_request_json,
                    expected_generation, result_generation,
                    result_reference, result_hash
             FROM engagement_tour_command_attempts
             ORDER BY expected_generation, result_generation, attempt_id",
        )
        .expect("command-shift statement")
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .expect("command-shift rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect command-shift rows");
    connection
        .execute_batch(
            "DROP TRIGGER engagement_tour_command_attempts_no_update;
             DROP TRIGGER engagement_tour_heads_guarded_update;",
        )
        .expect("open coherent generation-shift window");
    for command in commands {
        let expected_generation = command
            .3
            .checked_add(shift)
            .expect("shift expected generation");
        let result_generation = command
            .4
            .checked_add(shift)
            .expect("shift result generation");
        let mut request: serde_json::Value =
            serde_json::from_str(&command.2).expect("command request JSON");
        request
            .as_object_mut()
            .expect("command request object")
            .insert(
                "expectedEngagementGeneration".to_owned(),
                serde_json::json!(
                    u64::try_from(expected_generation).expect("positive expected generation")
                ),
            );
        let canonical_request =
            canonical_json_string(&request).expect("shifted canonical command request");
        let request_hash = test_sha256(canonical_request.as_bytes());
        let result_hash = match command.1.as_str() {
            "START_TOUR" | "ACCEPT_SOURCE" => command.6,
            "RESERVE_SOURCE" => canonical_hash(&serde_json::json!({
                "schemaVersion": "migration-stream/engagement/tour-command-result/v1",
                "commandKind": "RESERVE_SOURCE",
                "tourAttemptId": request
                    .get("tourAttemptId")
                    .and_then(serde_json::Value::as_str)
                    .expect("reserve tour attempt"),
                "preflightId": command.5,
                "generation": u64::try_from(result_generation)
                    .expect("positive reserve result generation"),
            }))
            .expect("shifted reserve result hash"),
            "LAUNCH_VALIDATION" => canonical_hash(&serde_json::json!({
                "schemaVersion": "migration-stream/engagement/tour-command-result/v1",
                "commandKind": "LAUNCH_VALIDATION",
                "tourAttemptId": request
                    .get("tourAttemptId")
                    .and_then(serde_json::Value::as_str)
                    .expect("launch tour attempt"),
                "runId": command.5,
                "sourceReuseId": request
                    .get("expectedReuseAuthorityId")
                    .and_then(serde_json::Value::as_str)
                    .expect("launch reuse ID"),
                "sourceReuseHash": request
                    .get("expectedReuseAuthorityHash")
                    .and_then(serde_json::Value::as_str)
                    .expect("launch reuse hash"),
                "generation": u64::try_from(result_generation)
                    .expect("positive launch result generation"),
            }))
            .expect("shifted launch result hash"),
            other => panic!("unexpected Tour command kind {other:?}"),
        };
        assert_eq!(
            connection
                .execute(
                    "UPDATE engagement_tour_command_attempts
                     SET canonical_request_json = ?1, request_hash = ?2,
                         expected_generation = ?3, result_generation = ?4,
                         result_hash = ?5
                     WHERE attempt_id = ?6",
                    params![
                        canonical_request,
                        request_hash,
                        expected_generation,
                        result_generation,
                        result_hash,
                        command.0,
                    ],
                )
                .expect("coherently shift command generation"),
            1
        );
    }
    assert_eq!(
        connection
            .execute(
                "UPDATE engagement_tour_heads
                 SET generation = generation + ?1",
                [shift],
            )
            .expect("coherently shift head generation"),
        1
    );
    connection
        .execute_batch(
            "CREATE TRIGGER engagement_tour_command_attempts_no_update
                 BEFORE UPDATE ON engagement_tour_command_attempts
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'engagement tour command attempts are immutable'
                     );
                 END;
             CREATE TRIGGER engagement_tour_heads_guarded_update
                 BEFORE UPDATE ON engagement_tour_heads
                 WHEN NEW.engagement_id IS NOT OLD.engagement_id
                   OR NEW.generation != OLD.generation + 1
                   OR (
                       OLD.active_tour_id IS NOT NULL
                       AND NEW.active_tour_id IS NULL
                   )
                   OR NEW.updated_at IS OLD.updated_at
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'engagement tour head transition is invalid'
                     );
                 END;",
        )
        .expect("restore coherent generation-shift guards");
}

#[test]
#[allow(clippy::too_many_lines)]
fn real_sales_tour_two_is_sample_free_durable_and_compares_four_one_one() {
    let fixture = complete_tour_one();
    let tour_one_journey = fixture.service.journey().expect("Tour 1 journey");
    assert_eq!(tour_one_journey.current_stage_id, JourneyStageId::Issues);
    assert_eq!(
        tour_one_journey.stages[5].status,
        JourneyStageStatus::Available
    );
    assert_eq!(
        tour_one_journey.stages[5].href.as_deref(),
        Some("/comparison")
    );
    assert_eq!(
        tour_one_journey.stages[7].unavailable_reason.as_deref(),
        Some(MULTI_ENTITY_DOWNSTREAM_REASON)
    );
    let (tour_attempt_id, start_generation, baseline_tour_id) =
        start_tour_two(&fixture, "tour-2-success");

    let stale = fixture
        .service
        .reserve_revised_preflight(&ReserveRevisedPreflightCommand {
            schema_version: RESERVE_REVISED_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "tour-2-stale-reserve".to_owned(),
            expected_engagement_generation: start_generation.saturating_sub(1),
            tour_attempt_id,
            source_name: "stale-tour-2.zip".to_owned(),
        })
        .expect_err("stale tour generation must fail before reservation");
    assert!(matches!(stale, KernelError::Conflict(_)));
    let connection =
        Connection::open(fixture.temporary.path().join("state.sqlite3")).expect("database");
    assert_eq!(
        count(&connection, "SELECT COUNT(*) FROM validation_runs"),
        1
    );
    drop(connection);

    let prepared = prepare_tour_two_after_start(&fixture, "tour-2-success", start_generation)
        .expect("accept structurally compatible Tour 2");
    let launch = launch_command(&prepared, "tour-2-success");
    let result = fixture
        .service
        .launch_revised_validation(&launch)
        .expect("launch compared Tour 2");
    assert_eq!(result.active_tour.tour_number, 2);
    assert_eq!(
        result.active_tour.predecessor_tour_id.as_deref(),
        Some(baseline_tour_id.as_str())
    );
    assert_eq!(result.comparison.closed_count, 4);
    assert_eq!(result.comparison.remaining_count, 1);
    assert_eq!(result.comparison.new_count, 1);
    assert_eq!(result.comparison.findings.len(), 6);
    assert_eq!(
        result
            .comparison
            .findings
            .iter()
            .filter(|finding| {
                finding.category == migration_stream::engagement::ComparedFindingCategory::Closed
            })
            .count(),
        4
    );
    assert_eq!(
        result
            .comparison
            .findings
            .iter()
            .filter(|finding| {
                finding.category == migration_stream::engagement::ComparedFindingCategory::Remaining
            })
            .count(),
        1
    );
    assert_eq!(
        result
            .comparison
            .findings
            .iter()
            .filter(|finding| {
                finding.category == migration_stream::engagement::ComparedFindingCategory::New
            })
            .count(),
        1
    );

    let replayed = fixture
        .service
        .launch_revised_validation(&launch)
        .expect("idempotent Tour 2 launch replay");
    assert!(replayed.idempotent_replay);
    assert_eq!(replayed.active_tour, result.active_tour);
    assert_eq!(replayed.comparison, result.comparison);

    let connection =
        Connection::open(fixture.temporary.path().join("state.sqlite3")).expect("database");
    let revised_counts: (i64, i64) = connection
        .query_row(
            "SELECT rows_processed, issue_count FROM validation_runs WHERE id = ?1",
            [&result.active_tour.validation_run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("revised validation counts");
    assert_eq!(revised_counts, (10, 2));
    let baseline_counts: (i64, i64) = connection
        .query_row(
            "SELECT rows_processed, issue_count FROM validation_runs WHERE id = ?1",
            [&fixture.validation_run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("immutable Tour 1 counts");
    assert_eq!(baseline_counts, (10, 5));
    let samples: Vec<String> = connection
        .prepare("SELECT samples_json FROM profiling_columns WHERE run_id = ?1")
        .expect("Tour 2 sample query")
        .query_map([&prepared.profiling_run_id], |row| row.get(0))
        .expect("Tour 2 profiling rows")
        .collect::<rusqlite::Result<_>>()
        .expect("Tour 2 samples");
    assert_eq!(samples.len(), 14);
    assert!(samples.iter().all(|sample| sample == "[]"));
    drop(connection);

    let chain = fixture.service.tour_chain().expect("completed chain");
    assert_eq!(chain.active_tour.as_ref(), Some(&result.active_tour));
    assert!(chain.pending_attempt.is_none());
    assert_eq!(chain.latest_comparison.as_ref(), Some(&result.comparison));
    assert_eq!(chain.tours.len(), 2);
    assert_eq!(chain.comparisons, vec![result.comparison.clone()]);
    assert_eq!(chain.tours[0].tour_number, 1);
    assert_eq!(chain.tours[1].tour_number, 2);
    let journey = fixture.service.journey().expect("completed Tour 2 journey");
    assert_eq!(journey.current_stage_id, JourneyStageId::Issues);
    assert_eq!(
        journey.binding.active_tour_id.as_deref(),
        Some(result.active_tour.tour_id.as_str())
    );
    assert_eq!(
        journey.binding.active_tour_hash.as_deref(),
        Some(result.active_tour.tour_hash.as_str())
    );
    assert_eq!(
        journey.binding.comparison_id.as_deref(),
        Some(result.comparison.comparison_id.as_str())
    );
    assert_eq!(
        journey.binding.comparison_hash.as_deref(),
        Some(result.comparison.comparison_hash.as_str())
    );
    assert_eq!(journey.binding.entity_count, Some(2));
    assert_eq!(journey.stages[5].status, JourneyStageStatus::Completed);
    assert_eq!(journey.stages[6].status, JourneyStageStatus::Current);
    assert!(journey.stages[7..].iter().all(|stage| {
        stage.status == JourneyStageStatus::Locked
            && stage.href.is_none()
            && stage.unavailable_reason.as_deref() == Some(MULTI_ENTITY_DOWNSTREAM_REASON)
    }));
    let restarted = sales_service(fixture.temporary.path());
    let restarted_chain = restarted.tour_chain().expect("restarted chain");
    assert_eq!(restarted_chain, chain);
    assert_eq!(
        restarted.journey().expect("restarted Tour 2 journey"),
        journey
    );

    let broad = serde_json::to_string(&result.comparison).expect("comparison JSON");
    assert!(broad.contains("identityBasis"));
    assert!(broad.contains("DECLARED_KEY") || broad.contains("UNIQUE_SCOPE"));
    for received in [
        "SD-1001",
        "SD-1002",
        "SD-9999",
        "SKU-E",
        "CUST-0042",
        "2026-13-10",
    ] {
        assert!(
            !broad.contains(received),
            "broad comparison leaked {received}"
        );
    }
    let integrity = integrity_check(fixture.temporary.path()).expect("Tour chain integrity");
    assert!(integrity.ok, "{:?}", integrity.issues);
    assert_eq!(integrity.checked_engagement_tour_heads, 1);
    assert_eq!(integrity.checked_validation_comparisons, 1);
}

#[test]
fn third_revision_compares_adjacent_multisets_and_replays_complete_history() {
    let fixture = complete_tour_one();
    let tour_two = complete_tour_two(&fixture, "tour-2-before-third");
    let before_third = fixture.service.tour_chain().expect("history before third");
    let prepared = prepare_revision(
        &fixture,
        "tour-3-reordered",
        "fixtures/sales-document-unfamiliar-tour-2",
        false,
        true,
    )
    .expect("prepare structurally compatible third revision");
    assert_eq!(
        prepared.baseline_source_hash,
        tour_two.active_tour.source_hash
    );
    assert_eq!(
        prepared.mapping_origin_source_hash,
        before_third.tours[0].source_hash
    );
    assert_ne!(
        prepared.mapping_origin_source_hash,
        prepared.baseline_source_hash
    );

    let tour_three = fixture
        .service
        .launch_revised_validation(&launch_command(&prepared, "tour-3-reordered"))
        .expect("complete third revision");
    assert_eq!(tour_three.active_tour.tour_number, 3);
    assert_eq!(
        tour_three.active_tour.predecessor_tour_id.as_deref(),
        Some(tour_two.active_tour.tour_id.as_str())
    );
    assert_eq!(
        tour_three.comparison.baseline_tour_id,
        tour_two.active_tour.tour_id
    );
    assert_eq!(
        tour_three.comparison.baseline_validation_run_id,
        tour_two.active_tour.validation_run_id
    );
    assert_eq!(tour_three.comparison.closed_count, 0);
    assert_eq!(tour_three.comparison.remaining_count, 2);
    assert_eq!(tour_three.comparison.new_count, 0);

    let chain = fixture
        .service
        .tour_chain()
        .expect("three-revision history");
    assert!(chain.pending_attempt.is_none());
    assert_eq!(chain.tours.len(), 3);
    assert_eq!(
        chain
            .tours
            .iter()
            .map(|tour| tour.tour_number)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(chain.comparisons.len(), 2);
    assert_eq!(
        chain.comparisons[0].candidate_tour_id,
        tour_two.active_tour.tour_id
    );
    assert_eq!(
        chain.comparisons[1].candidate_tour_id,
        tour_three.active_tour.tour_id
    );
    assert_eq!(
        chain.latest_comparison.as_ref(),
        Some(&chain.comparisons[1])
    );

    let restarted = sales_service(fixture.temporary.path());
    assert_eq!(restarted.tour_chain().expect("restarted history"), chain);
    let integrity = integrity_check(fixture.temporary.path()).expect("third revision integrity");
    assert!(integrity.ok, "{:?}", integrity.issues);
    assert_eq!(integrity.checked_validation_comparisons, 2);
}

#[test]
fn revised_validation_policy_drift_creates_no_run_or_comparison() {
    let fixture = complete_tour_one();
    let prepared =
        prepare_tour_two(&fixture, "tour-2-policy-drift", false).expect("prepared revision");
    let database_path = fixture.temporary.path().join("state.sqlite3");
    let before = Connection::open(&database_path).expect("database");
    let run_count = count(&before, "SELECT COUNT(*) FROM validation_runs");
    drop(before);

    let mut changed_policy =
        ValidationPolicy::from_path(&repository_path("config/engagement-validation-policy.json"))
            .expect("validation policy");
    changed_policy.row_batch_size = if changed_policy.row_batch_size > 1 {
        changed_policy.row_batch_size - 1
    } else {
        changed_policy.row_batch_size + 1
    };
    let changed =
        sales_service_with_policy(fixture.temporary.path(), vec![sales_pack()], changed_policy);
    let error = changed
        .launch_revised_validation(&launch_command(&prepared, "tour-2-policy-drift"))
        .expect_err("changed validation semantics must fail before run creation");
    assert!(matches!(error, KernelError::Conflict(_)));
    assert!(error.to_string().contains("policy, or engine differs"));

    let after = Connection::open(database_path).expect("database");
    assert_eq!(
        count(&after, "SELECT COUNT(*) FROM validation_runs"),
        run_count
    );
    assert_eq!(
        count(
            &after,
            "SELECT COUNT(*) FROM engagement_validation_comparisons"
        ),
        0
    );
    assert_eq!(
        count(
            &after,
            "SELECT COUNT(*) FROM engagement_tour_command_attempts
             WHERE command_kind = 'LAUNCH_VALIDATION'"
        ),
        0
    );
}

#[test]
fn downstream_authority_fails_closed_on_catalogue_absence_or_mode_drift() {
    let fixture = complete_tour_one();
    let missing = sales_service_with_packs(fixture.temporary.path(), vec![customer_pack()]);
    assert!(matches!(
        missing.context(),
        Err(KernelError::IntegrityFailed(_))
    ));
    assert!(matches!(
        missing.journey(),
        Err(KernelError::IntegrityFailed(_))
    ));

    let mut drifted_pack = sales_pack();
    drifted_pack.mapping_mode = MappingMode::ControlledSuggestions;
    drifted_pack
        .mapping_suggestions
        .push(ObjectPackMappingSuggestion {
            suggestion_id: "drifted-source-id".to_owned(),
            target_entity_id: Some(HEADER_ENTITY.to_owned()),
            target_field_id: "sourceDocumentId".to_owned(),
            source_file: Some("deal_book.csv".to_owned()),
            source_column: Some("case_ref".to_owned()),
            source_column_ordinal: Some(0),
            kind: MappingKind::Direct,
            constant: None,
            lookup_table_id: None,
        });
    let drifted = sales_service_with_packs(fixture.temporary.path(), vec![drifted_pack]);
    let context = drifted.context().expect("read drifted catalogue context");
    assert!(!context.capabilities.governed_issue_decisions);
    assert!(
        drifted
            .multi_entity_authority_gated()
            .expect("two-entity profile remains gated")
    );
    let journey = drifted.journey().expect("drifted catalogue journey");
    assert!(journey.stages[7..].iter().all(|stage| {
        stage.status == JourneyStageStatus::Locked
            && stage.href.is_none()
            && stage.unavailable_reason.as_deref() == Some(MULTI_ENTITY_DOWNSTREAM_REASON)
    }));

    let connection =
        Connection::open(fixture.temporary.path().join("state.sqlite3")).expect("database");
    connection
        .execute(
            "UPDATE engagement_active_context
             SET selected_object_pack_id = 'synthetic.customer.readiness'",
            [],
        )
        .expect("tamper pack/profile linkage");
    drop(connection);
    let incompatible = sales_service_with_packs(
        fixture.temporary.path(),
        vec![sales_pack(), customer_pack()],
    );
    assert!(matches!(
        incompatible.journey(),
        Err(KernelError::IntegrityFailed(_))
    ));
}

#[test]
fn journey_rejects_stale_active_validation_binding_against_tour_head() {
    let fixture = complete_tour_one();
    let result = complete_tour_two(&fixture, "journey-binding-tamper");
    assert_eq!(result.active_tour.tour_number, 2);
    let connection =
        Connection::open(fixture.temporary.path().join("state.sqlite3")).expect("database");
    connection
        .execute(
            "UPDATE engagement_active_context
             SET active_validation_run_id = ?1
             WHERE active_validation_run_id = ?2",
            params![
                fixture.validation_run_id,
                result.active_tour.validation_run_id
            ],
        )
        .expect("tamper active validation binding");
    drop(connection);
    assert!(matches!(
        fixture.service.journey(),
        Err(KernelError::IntegrityFailed(_))
    ));
}

fn prepare_tour_two_after_start(
    fixture: &CompletedTourOne,
    prefix: &str,
    start_generation: u64,
) -> Result<PreparedTourTwo, KernelError> {
    let chain = fixture.service.tour_chain()?;
    let tour_attempt_id = chain
        .pending_attempt
        .expect("pending started tour")
        .tour_attempt_id;
    let reservation_command = ReserveRevisedPreflightCommand {
        schema_version: RESERVE_REVISED_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
        idempotency_key: format!("{prefix}-reserve"),
        expected_engagement_generation: start_generation,
        tour_attempt_id: tour_attempt_id.clone(),
        source_name: format!("{prefix}.zip"),
    };
    let reservation = fixture
        .service
        .reserve_revised_preflight(&reservation_command)?;
    let replayed = fixture
        .service
        .reserve_revised_preflight(&reservation_command)?;
    assert_eq!(
        replayed.preflight.preflight_id,
        reservation.preflight.preflight_id
    );
    let after_reserve = fixture.service.tour_chain()?;
    let zip_path = fixture.temporary.path().join(format!("{prefix}.zip"));
    build_zip(
        &zip_path,
        "fixtures/sales-document-unfamiliar-tour-2",
        false,
    );
    fs::copy(&zip_path, &reservation.controlled_path).expect("stream revised source");
    let (source_hash, source_size) = file_identity(&zip_path);
    let preflight = fixture
        .service
        .complete_preflight(&CompletePreflightCommand {
            preflight_id: reservation.preflight.preflight_id,
            sha256: source_hash.clone(),
            size_bytes: source_size,
        })?;
    let command = AcceptRevisedPreflightCommand {
        schema_version: ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA.to_owned(),
        idempotency_key: format!("{prefix}-accept"),
        expected_engagement_generation: after_reserve.engagement_generation,
        tour_attempt_id: tour_attempt_id.clone(),
        preflight_id: preflight.preflight_id,
        expected_source_hash: source_hash,
        expected_manifest_hash: preflight.manifest_hash.expect("manifest"),
        actor: "tour-test-operator".to_owned(),
        reason: "Accept this exact synthetic revised source for compared validation.".to_owned(),
        acknowledgement: REVISED_SOURCE_ACKNOWLEDGEMENT.to_owned(),
    };
    let accepted = fixture.service.accept_revised_preflight(&command)?;
    let replayed_accept = fixture.service.accept_revised_preflight(&command)?;
    assert!(replayed_accept.idempotent_replay);
    Ok(PreparedTourTwo {
        tour_attempt_id,
        generation: accepted.engagement_generation,
        reuse_authority_id: accepted.reuse_authority.reuse_authority_id,
        reuse_authority_hash: accepted.reuse_authority.reuse_authority_hash,
        baseline_source_hash: accepted.reuse_authority.baseline_source_hash,
        mapping_origin_source_hash: accepted.reuse_authority.mapping_origin_source_hash,
        profiling_run_id: accepted
            .tour_attempt
            .profiling_run_id
            .expect("profiling run"),
    })
}

#[test]
#[allow(clippy::too_many_lines)]
fn header_drift_requires_new_mapping_and_never_advances_tour_one() {
    let fixture = complete_tour_one();
    let baseline_state = fixture.service.state().expect("baseline state");
    let (_, _, baseline_tour_id) = start_tour_two(&fixture, "tour-2-drift");
    let chain = fixture.service.tour_chain().expect("started chain");
    let attempt = chain.pending_attempt.expect("pending drift attempt");
    let reservation = fixture
        .service
        .reserve_revised_preflight(&ReserveRevisedPreflightCommand {
            schema_version: RESERVE_REVISED_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "tour-2-drift-reserve".to_owned(),
            expected_engagement_generation: chain.engagement_generation,
            tour_attempt_id: attempt.tour_attempt_id.clone(),
            source_name: "tour-2-drift.zip".to_owned(),
        })
        .expect("reserve drift source");
    let after_reserve = fixture.service.tour_chain().expect("reserved chain");
    let zip_path = fixture.temporary.path().join("tour-2-drift.zip");
    build_zip(&zip_path, "fixtures/sales-document-unfamiliar-tour-2", true);
    fs::copy(&zip_path, &reservation.controlled_path).expect("stream drift source");
    let (source_hash, source_size) = file_identity(&zip_path);
    let preflight = fixture
        .service
        .complete_preflight(&CompletePreflightCommand {
            preflight_id: reservation.preflight.preflight_id,
            sha256: source_hash.clone(),
            size_bytes: source_size,
        })
        .expect("complete drift preflight");
    let accept = AcceptRevisedPreflightCommand {
        schema_version: ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA.to_owned(),
        idempotency_key: "tour-2-drift-accept".to_owned(),
        expected_engagement_generation: after_reserve.engagement_generation,
        tour_attempt_id: attempt.tour_attempt_id,
        preflight_id: preflight.preflight_id,
        expected_source_hash: source_hash,
        expected_manifest_hash: preflight.manifest_hash.expect("drift manifest"),
        actor: "tour-test-operator".to_owned(),
        reason: "Check exact structural compatibility before reuse.".to_owned(),
        acknowledgement: REVISED_SOURCE_ACKNOWLEDGEMENT.to_owned(),
    };
    let error = fixture
        .service
        .accept_revised_preflight(&accept)
        .expect_err("header drift must require a new mapping");
    assert!(matches!(error, KernelError::Conflict(_)));
    assert!(error.to_string().contains("new governed mapping"));
    let retry = fixture
        .service
        .accept_revised_preflight(&accept)
        .expect_err("mapping-required result is durably idempotent");
    assert!(retry.to_string().contains("new governed mapping"));

    let final_chain = fixture.service.tour_chain().expect("drift chain");
    assert_eq!(
        final_chain
            .active_tour
            .expect("Tour 1 remains active")
            .tour_id,
        baseline_tour_id
    );
    let pending = final_chain
        .pending_attempt
        .expect("mapping-required revision remains pending");
    assert_eq!(
        pending.state,
        migration_stream::engagement::RevisedTourAttemptState::Incompatible
    );
    assert_eq!(pending.error_code.as_deref(), Some("MAPPING_REQUIRED"));
    assert_eq!(
        pending.source_hash.as_deref(),
        Some(accept.expected_source_hash.as_str())
    );
    assert!(pending.job_id.is_some());
    assert!(pending.profiling_run_id.is_some());
    assert!(pending.structure_hash.is_some());
    let candidate_job_id = pending.job_id.clone().expect("candidate job");
    let candidate_source_hash = pending.source_hash.clone().expect("candidate source");
    let candidate_profiling_run_id = pending
        .profiling_run_id
        .clone()
        .expect("candidate profiling");
    let mapping_workspace = fixture
        .service
        .mapping_workspace()
        .expect("mapping-required workspace");
    assert_eq!(mapping_workspace.job_id, candidate_job_id);
    assert_eq!(mapping_workspace.source_hash, candidate_source_hash);
    assert_eq!(
        mapping_workspace.profiling_run_id,
        candidate_profiling_run_id
    );
    let revision_scope = mapping_workspace
        .revision_draft_scope
        .as_ref()
        .expect("revision draft scope");
    assert_eq!(revision_scope.tour_attempt_id, pending.tour_attempt_id);
    assert_eq!(revision_scope.tour_number, 2);
    assert_eq!(revision_scope.baseline_tour_id, baseline_tour_id);
    assert!(!revision_scope.submission_available);

    let mut revised_snapshot = governed_snapshot();
    let selling_unit = revised_snapshot
        .bindings
        .iter_mut()
        .find(|binding| binding.binding_id == "header.selling-unit")
        .expect("selling-unit binding");
    selling_unit.source_column = Some("unexpected_desk".to_owned());
    let saved = fixture
        .service
        .save_mapping_draft(&SaveMappingDraftCommand {
            schema_version: SAVE_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "tour-2-drift-save-draft".to_owned(),
            expected_engagement_generation: mapping_workspace.engagement_generation,
            expected_draft_id: None,
            expected_draft_generation: None,
            expected_draft_hash: None,
            snapshot: revised_snapshot,
        })
        .expect("save candidate-keyed revision draft");
    assert_eq!(
        saved.engagement_generation,
        mapping_workspace.engagement_generation
    );
    let after_save_state = fixture.service.state().expect("state after revision draft");
    assert_eq!(after_save_state.generation, baseline_state.generation);
    assert_eq!(after_save_state.active.job_id, baseline_state.active.job_id);
    assert_eq!(
        after_save_state.active.mapping_id,
        baseline_state.active.mapping_id
    );
    assert_eq!(
        after_save_state.active.validation_run_id,
        baseline_state.active.validation_run_id
    );

    assert!(final_chain.latest_comparison.is_none());
    let connection =
        Connection::open(fixture.temporary.path().join("state.sqlite3")).expect("database");
    let mapping_versions_before = count(&connection, "SELECT COUNT(*) FROM mapping_versions");
    drop(connection);
    let submit = fixture
        .service
        .submit_mapping_draft(&SubmitMappingDraftCommand {
            schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "tour-2-drift-submit-blocked".to_owned(),
            expected_engagement_generation: saved.engagement_generation,
            draft_id: saved.draft.draft_id,
            expected_draft_generation: saved.draft.draft_generation,
            expected_draft_hash: saved.draft.draft_hash,
            mapping_id: "synthetic.sales-document.revision-drift".to_owned(),
            version: "1.0.0".to_owned(),
            acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect_err("revision draft submission remains fail-closed");
    assert!(matches!(submit, KernelError::Conflict(_)));
    let connection =
        Connection::open(fixture.temporary.path().join("state.sqlite3")).expect("database");
    assert_eq!(
        count(&connection, "SELECT COUNT(*) FROM mapping_versions"),
        mapping_versions_before
    );
    assert_eq!(
        count(&connection, "SELECT COUNT(*) FROM validation_runs"),
        1
    );
    let active_run: String = connection
        .query_row(
            "SELECT active_validation_run_id FROM engagement_active_context",
            [],
            |row| row.get(0),
        )
        .expect("active Tour 1 validation");
    assert_eq!(active_run, fixture.validation_run_id);
}

#[test]
fn tampered_reuse_canonical_authority_creates_no_validation_run_or_activation() {
    let fixture = complete_tour_one();
    let prepared = prepare_tour_two(&fixture, "tour-2-tamper", false).expect("prepared Tour 2");
    let before = fixture.service.tour_chain().expect("chain before tamper");
    let baseline_tour_id = before.active_tour.expect("active Tour 1").tour_id;
    let database_path = fixture.temporary.path().join("state.sqlite3");
    let connection = Connection::open(&database_path).expect("tamper database");
    assert_eq!(
        count(&connection, "SELECT COUNT(*) FROM validation_runs"),
        1
    );
    connection
        .execute_batch(
            "DROP TRIGGER engagement_mapping_reuse_authorities_no_update;
             UPDATE engagement_mapping_reuse_authorities
             SET canonical_reuse_json = '{}';
             CREATE TRIGGER engagement_mapping_reuse_authorities_no_update
                 BEFORE UPDATE ON engagement_mapping_reuse_authorities
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'mapping source-reuse authorities are immutable'
                     );
                 END;",
        )
        .expect("tamper reuse authority behind restored guard");
    drop(connection);

    let error = fixture
        .service
        .launch_revised_validation(&launch_command(&prepared, "tour-2-tamper"))
        .expect_err("tampered reuse authority must fail before validation");
    assert!(
        matches!(
            error,
            KernelError::IntegrityFailed(_) | KernelError::Json(_)
        ),
        "{error}"
    );
    let connection = Connection::open(database_path).expect("post-tamper database");
    assert_eq!(
        count(&connection, "SELECT COUNT(*) FROM validation_runs"),
        1
    );
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_validation_comparisons"
        ),
        0
    );
    let chain = fixture
        .service
        .tour_chain()
        .expect_err("tamper is visible on read");
    assert!(chain.to_string().contains("integrity") || chain.to_string().contains("JSON"));
    let active_tour: String = connection
        .query_row(
            "SELECT active_tour_id FROM engagement_tour_heads",
            [],
            |row| row.get(0),
        )
        .expect("active tour head");
    assert_eq!(active_tour, baseline_tour_id);
    let active_run: String = connection
        .query_row(
            "SELECT active_validation_run_id FROM engagement_active_context",
            [],
            |row| row.get(0),
        )
        .expect("active validation");
    assert_eq!(active_run, fixture.validation_run_id);
}

#[test]
#[allow(clippy::too_many_lines, clippy::type_complexity)]
fn same_generation_mapping_context_tamper_cannot_activate_candidate_tour() {
    let fixture = complete_tour_one();
    let prepared =
        prepare_tour_two(&fixture, "tour-2-active-context-tamper", false).expect("prepared Tour 2");
    let database_path = fixture.temporary.path().join("state.sqlite3");
    let connection = Connection::open(&database_path).expect("tamper database");
    let before_head: (i64, String, String) = connection
        .query_row(
            "SELECT generation, active_tour_id, pending_tour_attempt_id
             FROM engagement_tour_heads",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("pending tour head before context tamper");
    let before_context: (
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
        String,
        String,
        String,
    ) = connection
        .query_row(
            "SELECT generation, current_preflight_id, active_job_id,
                    active_profiling_run_id, selected_profile_id,
                    selected_profile_version, selected_profile_hash,
                    active_mapping_id, active_mapping_version,
                    active_mapping_hash, active_validation_run_id,
                    mapping_stage, validation_stage
             FROM engagement_active_context",
            [],
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
                ))
            },
        )
        .expect("baseline active context before tamper");
    let counts_before = (
        count(&connection, "SELECT COUNT(*) FROM validation_runs"),
        count(&connection, "SELECT COUNT(*) FROM engagement_tours"),
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_tour_finding_sets",
        ),
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_validation_comparisons",
        ),
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_tour_command_attempts
             WHERE command_kind = 'LAUNCH_VALIDATION'",
        ),
    );
    let tampered_mapping_hash = "f".repeat(64);
    assert_ne!(before_context.9, tampered_mapping_hash);
    connection
        .execute(
            "UPDATE engagement_active_context
             SET active_mapping_hash = ?1
             WHERE generation = ?2 AND active_validation_run_id = ?3",
            params![tampered_mapping_hash, before_context.0, before_context.10],
        )
        .expect("tamper same-generation non-run active authority");
    drop(connection);

    let error = fixture
        .service
        .launch_revised_validation(&launch_command(&prepared, "tour-2-active-context-tamper"))
        .expect_err("same-generation mapping tamper must fail before launch planning");
    assert!(matches!(error, KernelError::IntegrityFailed(_)), "{error}");
    assert!(error.to_string().contains("active engagement context"));

    let connection = Connection::open(database_path).expect("post-tamper database");
    let after_head: (i64, String, String) = connection
        .query_row(
            "SELECT generation, active_tour_id, pending_tour_attempt_id
             FROM engagement_tour_heads",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("pending tour head after rejected activation");
    assert_eq!(after_head, before_head);
    let after_context: (
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
        String,
        String,
        String,
    ) = connection
        .query_row(
            "SELECT generation, current_preflight_id, active_job_id,
                    active_profiling_run_id, selected_profile_id,
                    selected_profile_version, selected_profile_hash,
                    active_mapping_id, active_mapping_version,
                    active_mapping_hash, active_validation_run_id,
                    mapping_stage, validation_stage
             FROM engagement_active_context",
            [],
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
                ))
            },
        )
        .expect("active context after rejected activation");
    assert_eq!(after_context.0, before_context.0);
    assert_eq!(
        (
            &after_context.1,
            &after_context.2,
            &after_context.3,
            &after_context.4,
            &after_context.5,
            &after_context.6,
            &after_context.7,
            &after_context.8,
        ),
        (
            &before_context.1,
            &before_context.2,
            &before_context.3,
            &before_context.4,
            &before_context.5,
            &before_context.6,
            &before_context.7,
            &before_context.8,
        ),
    );
    assert_eq!(after_context.9, "f".repeat(64));
    assert_eq!(
        (&after_context.10, &after_context.11, &after_context.12),
        (&before_context.10, &before_context.11, &before_context.12),
    );
    assert_eq!(
        (
            count(&connection, "SELECT COUNT(*) FROM validation_runs"),
            count(&connection, "SELECT COUNT(*) FROM engagement_tours"),
            count(
                &connection,
                "SELECT COUNT(*) FROM engagement_tour_finding_sets",
            ),
            count(
                &connection,
                "SELECT COUNT(*) FROM engagement_validation_comparisons",
            ),
            count(
                &connection,
                "SELECT COUNT(*) FROM engagement_tour_command_attempts
                 WHERE command_kind = 'LAUNCH_VALIDATION'",
            ),
        ),
        counts_before,
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn concurrent_launches_create_exactly_one_tour_and_comparison() {
    let fixture = complete_tour_one();
    let prepared =
        prepare_tour_two(&fixture, "tour-2-launch-race", false).expect("prepared Tour 2");
    let mut first_command = launch_command(&prepared, "tour-2-launch-race-a");
    first_command.idempotency_key = "tour-2-launch-race-command-a".to_owned();
    let mut second_command = launch_command(&prepared, "tour-2-launch-race-b");
    second_command.idempotency_key = "tour-2-launch-race-command-b".to_owned();

    let barrier = Arc::new(Barrier::new(3));
    let first_service = fixture.service.clone();
    let first_barrier = Arc::clone(&barrier);
    let first_retry = first_command.clone();
    let first = thread::spawn(move || {
        first_barrier.wait();
        first_service.launch_revised_validation(&first_command)
    });
    let second_service = fixture.service.clone();
    let second_barrier = Arc::clone(&barrier);
    let second_retry = second_command.clone();
    let second = thread::spawn(move || {
        second_barrier.wait();
        second_service.launch_revised_validation(&second_command)
    });
    barrier.wait();

    let first_result = first.join().expect("first launch thread");
    let second_result = second.join().expect("second launch thread");
    let success_count = usize::from(first_result.is_ok()) + usize::from(second_result.is_ok());
    assert_eq!(success_count, 1, "exactly one competing launch may commit");
    let losing_command = if first_result.is_err() {
        first_retry
    } else {
        second_retry
    };
    let losing_error = if let Err(error) = first_result {
        error
    } else {
        second_result.expect_err("second launch must lose")
    };
    assert!(
        matches!(
            losing_error,
            KernelError::Conflict(_) | KernelError::WorkspaceLocked(_)
        ),
        "{losing_error}"
    );

    let chain = fixture.service.tour_chain().expect("completed race chain");
    assert_eq!(chain.active_tour.expect("active Tour 2").tour_number, 2);
    assert!(chain.pending_attempt.is_none());
    assert!(chain.latest_comparison.is_some());
    let connection =
        Connection::open(fixture.temporary.path().join("state.sqlite3")).expect("database");
    assert_eq!(
        count(&connection, "SELECT COUNT(*) FROM validation_runs"),
        2
    );
    assert_eq!(
        count(&connection, "SELECT COUNT(*) FROM engagement_tours"),
        2
    );
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_tour_finding_sets"
        ),
        2
    );
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_validation_comparisons"
        ),
        1
    );
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_tour_command_attempts
             WHERE command_kind = 'LAUNCH_VALIDATION'"
        ),
        1
    );
    drop(connection);

    let retry_error = fixture
        .service
        .launch_revised_validation(&losing_command)
        .expect_err("stale losing launch may not create an orphan candidate");
    assert!(matches!(retry_error, KernelError::Conflict(_)));
    let connection =
        Connection::open(fixture.temporary.path().join("state.sqlite3")).expect("database");
    assert_eq!(
        count(&connection, "SELECT COUNT(*) FROM validation_runs"),
        2
    );
    assert_eq!(
        count(&connection, "SELECT COUNT(*) FROM engagement_tours"),
        2
    );
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_validation_comparisons"
        ),
        1
    );
}

struct IntegrityTamperCase {
    name: &'static str,
    sql: &'static str,
    expected_issue: &'static str,
}

#[test]
#[allow(clippy::too_many_lines)]
fn global_integrity_rejects_tour_and_comparison_chain_tampering() {
    let cases = [
        IntegrityTamperCase {
            name: "active tour linkage",
            sql: "
                DROP TRIGGER engagement_tour_heads_guarded_update;
                UPDATE engagement_tour_heads
                SET active_tour_id = (
                    SELECT tour_id FROM engagement_tours
                    WHERE tour_number = 1
                );
                CREATE TRIGGER engagement_tour_heads_guarded_update
                    BEFORE UPDATE ON engagement_tour_heads
                    WHEN NEW.engagement_id IS NOT OLD.engagement_id
                      OR NEW.generation != OLD.generation + 1
                      OR (
                          OLD.active_tour_id IS NOT NULL
                          AND NEW.active_tour_id IS NULL
                      )
                      OR NEW.updated_at IS OLD.updated_at
                    BEGIN
                        SELECT RAISE(
                            ABORT,
                            'engagement tour head transition is invalid'
                        );
                    END;",
            expected_issue: "active engagement context differs from immutable active tour",
        },
        IntegrityTamperCase {
            name: "coherent active tour rollback",
            sql: "
                DROP TRIGGER engagement_tour_heads_guarded_update;
                UPDATE engagement_tour_heads
                SET active_tour_id = (
                    SELECT tour_id FROM engagement_tours
                    WHERE tour_number = 1
                );
                CREATE TRIGGER engagement_tour_heads_guarded_update
                    BEFORE UPDATE ON engagement_tour_heads
                    WHEN NEW.engagement_id IS NOT OLD.engagement_id
                      OR NEW.generation != OLD.generation + 1
                      OR (
                          OLD.active_tour_id IS NOT NULL
                          AND NEW.active_tour_id IS NULL
                      )
                      OR NEW.updated_at IS OLD.updated_at
                    BEGIN
                        SELECT RAISE(
                            ABORT,
                            'engagement tour head transition is invalid'
                        );
                    END;
                UPDATE engagement_active_context
                SET current_preflight_id = (
                        SELECT preflight_id FROM engagement_tours
                        WHERE tour_number = 1
                    ),
                    active_job_id = (
                        SELECT job_id FROM engagement_tours
                        WHERE tour_number = 1
                    ),
                    active_profiling_run_id = (
                        SELECT profiling_run_id FROM engagement_tours
                        WHERE tour_number = 1
                    ),
                    selected_profile_hash = (
                        SELECT profile_hash FROM engagement_tours
                        WHERE tour_number = 1
                    ),
                    active_mapping_hash = (
                        SELECT mapping_hash FROM engagement_tours
                        WHERE tour_number = 1
                    ),
                    active_validation_run_id = (
                        SELECT validation_run_id FROM engagement_tours
                        WHERE tour_number = 1
                    ),
                    validation_stage = 'COMPLETE',
                    generation = generation - 1;",
            expected_issue: "tour head does not reference the terminal immutable registered tour",
        },
        IntegrityTamperCase {
            name: "tour finding membership",
            sql: "
                DROP TRIGGER engagement_tour_finding_members_no_update;
                UPDATE engagement_tour_finding_members
                SET member_hash =
                    '0000000000000000000000000000000000000000000000000000000000000000'
                WHERE rowid = (
                    SELECT rowid FROM engagement_tour_finding_members
                    ORDER BY finding_set_id, finding_fingerprint, occurrence_index
                    LIMIT 1
                );
                CREATE TRIGGER engagement_tour_finding_members_no_update
                    BEFORE UPDATE ON engagement_tour_finding_members
                    BEGIN
                        SELECT RAISE(ABORT, 'tour finding members are immutable');
                    END;",
            expected_issue: "differs from independent finding-set replay",
        },
        IntegrityTamperCase {
            name: "comparison membership",
            sql: "
                DROP TRIGGER engagement_validation_comparison_items_no_delete;
                DELETE FROM engagement_validation_comparison_items
                WHERE rowid = (
                    SELECT rowid FROM engagement_validation_comparison_items
                    ORDER BY comparison_id, category, finding_fingerprint,
                             category_occurrence_index
                    LIMIT 1
                );
                CREATE TRIGGER engagement_validation_comparison_items_no_delete
                    BEFORE DELETE ON engagement_validation_comparison_items
                    BEGIN
                        SELECT RAISE(
                            ABORT,
                            'validation comparison items are immutable'
                        );
                    END;",
            expected_issue: "comparison item storage omits canonical comparison occurrences",
        },
        IntegrityTamperCase {
            name: "deleted comparison graph",
            sql: "
                DROP TRIGGER engagement_validation_comparison_items_no_delete;
                DROP TRIGGER engagement_validation_comparisons_no_delete;
                DELETE FROM engagement_validation_comparison_items;
                DELETE FROM engagement_validation_comparisons;
                CREATE TRIGGER engagement_validation_comparisons_no_delete
                    BEFORE DELETE ON engagement_validation_comparisons
                    BEGIN
                        SELECT RAISE(ABORT, 'validation comparisons are immutable');
                    END;
                CREATE TRIGGER engagement_validation_comparison_items_no_delete
                    BEFORE DELETE ON engagement_validation_comparison_items
                    BEGIN
                        SELECT RAISE(ABORT, 'validation comparison items are immutable');
                    END;",
            expected_issue: "comparison history does not cover every immutable adjacent tour pair",
        },
        IntegrityTamperCase {
            name: "comparison persisted count",
            sql: "
                DROP TRIGGER engagement_validation_comparisons_no_update;
                UPDATE engagement_validation_comparisons
                SET closed_count = closed_count + 1;
                CREATE TRIGGER engagement_validation_comparisons_no_update
                    BEFORE UPDATE ON engagement_validation_comparisons
                    BEGIN
                        SELECT RAISE(ABORT, 'validation comparisons are immutable');
                    END;",
            expected_issue: "comparison persisted authority/count/linkage differs",
        },
        IntegrityTamperCase {
            name: "comparison tour linkage",
            sql: "
                DROP TRIGGER engagement_validation_comparisons_no_update;
                UPDATE engagement_validation_comparisons
                SET baseline_tour_id = revised_tour_id,
                    revised_tour_id = baseline_tour_id;
                CREATE TRIGGER engagement_validation_comparisons_no_update
                    BEFORE UPDATE ON engagement_validation_comparisons
                    BEGIN
                        SELECT RAISE(ABORT, 'validation comparisons are immutable');
                    END;",
            expected_issue: "comparison does not bind adjacent immutable revisions",
        },
        IntegrityTamperCase {
            name: "comparison algorithm",
            sql: "
                DROP TRIGGER engagement_validation_comparisons_no_update;
                PRAGMA ignore_check_constraints = ON;
                UPDATE engagement_validation_comparisons
                SET algorithm_version =
                    'migration-stream/compared-validation-fingerprint/v0';
                PRAGMA ignore_check_constraints = OFF;
                CREATE TRIGGER engagement_validation_comparisons_no_update
                    BEFORE UPDATE ON engagement_validation_comparisons
                    BEGIN
                        SELECT RAISE(ABORT, 'validation comparisons are immutable');
                    END;",
            expected_issue: "comparison persisted authority/count/linkage differs",
        },
        IntegrityTamperCase {
            name: "comparison namespace secret",
            sql: "
                DROP TRIGGER engagement_comparison_namespaces_no_update;
                UPDATE engagement_comparison_namespaces
                SET secret = zeroblob(32);
                CREATE TRIGGER engagement_comparison_namespaces_no_update
                    BEFORE UPDATE ON engagement_comparison_namespaces
                    BEGIN
                        SELECT RAISE(
                            ABORT,
                            'comparison identity namespaces are immutable'
                        );
                    END;",
            expected_issue: "comparison namespace replay failed",
        },
    ];

    let mut failures = Vec::new();
    for case in cases {
        let fixture = complete_tour_one();
        complete_tour_two(
            &fixture,
            &format!("tour-2-integrity-{}", case.name.replace(' ', "-")),
        );
        let baseline = integrity_check(fixture.temporary.path()).expect("clean integrity baseline");
        assert!(baseline.ok, "{}: {:?}", case.name, baseline.issues);

        let connection =
            Connection::open(fixture.temporary.path().join("state.sqlite3")).expect("database");
        connection
            .execute_batch(case.sql)
            .unwrap_or_else(|error| panic!("{} mutation: {error}", case.name));
        drop(connection);

        let report =
            integrity_check(fixture.temporary.path()).expect("tampered integrity report completes");
        if report.ok {
            failures.push(format!("{} tamper escaped replay", case.name));
        } else if !report
            .issues
            .iter()
            .any(|issue| issue.contains(case.expected_issue))
        {
            failures.push(format!(
                "{} did not report {:?}: {:?}",
                case.name, case.expected_issue, report.issues
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[derive(Clone, Debug)]
struct StoredComparisonItem {
    comparison_id: String,
    category: String,
    fingerprint: String,
    category_index: i64,
    baseline_set_id: Option<String>,
    baseline_occurrence: Option<i64>,
    baseline_member_hash: Option<String>,
    revised_set_id: Option<String>,
    revised_occurrence: Option<i64>,
    revised_member_hash: Option<String>,
    explanation_code: String,
}

fn stored_comparison_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredComparisonItem> {
    Ok(StoredComparisonItem {
        comparison_id: row.get(0)?,
        category: row.get(1)?,
        fingerprint: row.get(2)?,
        category_index: row.get(3)?,
        baseline_set_id: row.get(4)?,
        baseline_occurrence: row.get(5)?,
        baseline_member_hash: row.get(6)?,
        revised_set_id: row.get(7)?,
        revised_occurrence: row.get(8)?,
        revised_member_hash: row.get(9)?,
        explanation_code: row.get(10)?,
    })
}

fn comparison_membership_value(
    set_id: Option<&str>,
    occurrence: Option<i64>,
    member_hash: Option<&str>,
) -> Option<serde_json::Value> {
    match (set_id, occurrence, member_hash) {
        (Some(set_id), Some(occurrence), Some(member_hash)) => Some(serde_json::json!({
            "findingSetId": set_id,
            "occurrenceIndex": u64::try_from(occurrence)
                .expect("positive comparison occurrence"),
            "memberHash": member_hash,
        })),
        (None, None, None) => None,
        _ => panic!("comparison membership is incomplete"),
    }
}

fn canonical_comparison_item(item: &StoredComparisonItem) -> String {
    canonical_json_string(&serde_json::json!({
        "contract": "migration-stream/compared-validation/item/v1",
        "comparisonId": item.comparison_id,
        "category": item.category,
        "findingFingerprint": item.fingerprint,
        "categoryOccurrenceIndex": u64::try_from(item.category_index)
            .expect("positive category occurrence"),
        "baseline": comparison_membership_value(
            item.baseline_set_id.as_deref(),
            item.baseline_occurrence,
            item.baseline_member_hash.as_deref(),
        ),
        "revised": comparison_membership_value(
            item.revised_set_id.as_deref(),
            item.revised_occurrence,
            item.revised_member_hash.as_deref(),
        ),
        "explanationCode": item.explanation_code,
    }))
    .expect("canonical comparison item")
}

fn test_sha256(bytes: &[u8]) -> String {
    format!("{:064x}", Sha256::digest(bytes))
}

fn recompute_membership_hash(connection: &Connection, finding_set_id: &str) -> String {
    let members = connection
        .prepare(
            "SELECT finding_fingerprint, occurrence_index, member_hash
             FROM engagement_tour_finding_members
             WHERE finding_set_id = ?1
             ORDER BY finding_fingerprint, occurrence_index",
        )
        .expect("membership statement")
        .query_map([finding_set_id], |row| {
            Ok(serde_json::json!({
                "fingerprint": row.get::<_, String>(0)?,
                "occurrence": u64::try_from(row.get::<_, i64>(1)?)
                    .expect("positive finding occurrence"),
                "memberHash": row.get::<_, String>(2)?,
            }))
        })
        .expect("membership rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect membership");
    canonical_hash(&serde_json::json!({
        "contract": "migration-stream/compared-validation/finding-membership/v1",
        "findingSetId": finding_set_id,
        "members": members,
    }))
    .expect("canonical membership hash")
}

fn rehash_comparison_items_for_member(
    connection: &Connection,
    finding_set_id: &str,
    fingerprint: &str,
    occurrence: i64,
    member_hash: &str,
) -> usize {
    connection
        .execute(
            "UPDATE engagement_validation_comparison_items
             SET baseline_member_hash = ?1
             WHERE baseline_finding_set_id = ?2
               AND finding_fingerprint = ?3
               AND baseline_occurrence_index = ?4",
            params![member_hash, finding_set_id, fingerprint, occurrence],
        )
        .expect("relink baseline comparison member");
    connection
        .execute(
            "UPDATE engagement_validation_comparison_items
             SET revised_member_hash = ?1
             WHERE revised_finding_set_id = ?2
               AND finding_fingerprint = ?3
               AND revised_occurrence_index = ?4",
            params![member_hash, finding_set_id, fingerprint, occurrence],
        )
        .expect("relink revised comparison member");
    let items = connection
        .prepare(
            "SELECT comparison_id, category, finding_fingerprint,
                    category_occurrence_index,
                    baseline_finding_set_id, baseline_occurrence_index,
                    baseline_member_hash, revised_finding_set_id,
                    revised_occurrence_index, revised_member_hash,
                    explanation_code
             FROM engagement_validation_comparison_items
             WHERE (
                    baseline_finding_set_id = ?1
                AND finding_fingerprint = ?2
                AND baseline_occurrence_index = ?3
             ) OR (
                    revised_finding_set_id = ?1
                AND finding_fingerprint = ?2
                AND revised_occurrence_index = ?3
             )
             ORDER BY comparison_id, category, finding_fingerprint,
                      category_occurrence_index",
        )
        .expect("affected comparison-item statement")
        .query_map(
            params![finding_set_id, fingerprint, occurrence],
            stored_comparison_item,
        )
        .expect("affected comparison-item rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect affected comparison items");
    for item in &items {
        let canonical = canonical_comparison_item(item);
        let hash = test_sha256(canonical.as_bytes());
        let changed = connection
            .execute(
                "UPDATE engagement_validation_comparison_items
                 SET canonical_item_json = ?1, item_hash = ?2
                 WHERE comparison_id = ?3 AND category = ?4
                   AND finding_fingerprint = ?5
                   AND category_occurrence_index = ?6",
                params![
                    canonical,
                    hash,
                    item.comparison_id,
                    item.category,
                    item.fingerprint,
                    item.category_index,
                ],
            )
            .expect("rehash affected comparison item");
        assert_eq!(changed, 1);
    }
    items.len()
}

#[test]
#[allow(clippy::too_many_lines)]
fn coherent_finding_member_rehash_cannot_override_the_canonical_finding_set() {
    let fixture = complete_tour_one();
    complete_tour_two(&fixture, "tour-2-coherent-member");
    let baseline = integrity_check(fixture.temporary.path()).expect("clean integrity baseline");
    assert!(baseline.ok, "{:?}", baseline.issues);
    let database_path = fixture.temporary.path().join("state.sqlite3");
    let connection = Connection::open(&database_path).expect("database");
    let (finding_set_id, fingerprint, occurrence, source_ordinal, member_json): (
        String,
        String,
        i64,
        i64,
        String,
    ) = connection
        .query_row(
            "SELECT m.finding_set_id, m.finding_fingerprint,
                    m.occurrence_index, m.source_ordinal,
                    m.canonical_member_json
             FROM engagement_tour_finding_members m
             JOIN engagement_tour_finding_sets s
               ON s.finding_set_id = m.finding_set_id
             JOIN engagement_tours t ON t.tour_id = s.tour_id
             WHERE t.tour_number = 1
             ORDER BY m.finding_fingerprint, m.occurrence_index
             LIMIT 1",
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
        .expect("baseline finding member");
    let changed_ordinal = source_ordinal.checked_add(100).expect("source ordinal");
    let mut member_value: serde_json::Value =
        serde_json::from_str(&member_json).expect("member JSON");
    member_value.as_object_mut().expect("member object").insert(
        "sourceOrdinal".to_owned(),
        serde_json::json!(u64::try_from(changed_ordinal).expect("positive changed source ordinal")),
    );
    let changed_member_json =
        canonical_json_string(&member_value).expect("changed canonical member");
    let changed_member_hash = test_sha256(changed_member_json.as_bytes());

    connection
        .execute_batch(
            "DROP TRIGGER engagement_tour_finding_members_no_update;
             DROP TRIGGER engagement_tour_finding_sets_no_update;
             DROP TRIGGER engagement_validation_comparison_items_no_update;
             DROP TRIGGER engagement_validation_comparisons_no_update;",
        )
        .expect("open coherent member tamper window");
    let changed = connection
        .execute(
            "UPDATE engagement_tour_finding_members
             SET source_ordinal = ?1, canonical_member_json = ?2,
                 member_hash = ?3
             WHERE finding_set_id = ?4 AND finding_fingerprint = ?5
               AND occurrence_index = ?6",
            params![
                changed_ordinal,
                changed_member_json,
                changed_member_hash,
                finding_set_id,
                fingerprint,
                occurrence,
            ],
        )
        .expect("coherently alter member");
    assert_eq!(changed, 1);
    let changed_membership_hash = recompute_membership_hash(&connection, &finding_set_id);
    assert_eq!(
        connection
            .execute(
                "UPDATE engagement_tour_finding_sets
                 SET membership_hash = ?1 WHERE finding_set_id = ?2",
                params![changed_membership_hash, finding_set_id],
            )
            .expect("rehash finding-set membership"),
        1
    );
    let affected_items = rehash_comparison_items_for_member(
        &connection,
        &finding_set_id,
        &fingerprint,
        occurrence,
        &changed_member_hash,
    );
    assert!(affected_items > 0);
    assert!(
        connection
            .execute(
                "UPDATE engagement_validation_comparisons
                 SET baseline_membership_hash = CASE
                         WHEN baseline_finding_set_id = ?1 THEN ?2
                         ELSE baseline_membership_hash
                     END,
                     revised_membership_hash = CASE
                         WHEN revised_finding_set_id = ?1 THEN ?2
                         ELSE revised_membership_hash
                     END
                 WHERE baseline_finding_set_id = ?1
                    OR revised_finding_set_id = ?1",
                params![finding_set_id, changed_membership_hash],
            )
            .expect("relink comparison membership")
            > 0
    );
    connection
        .execute_batch(
            "CREATE TRIGGER engagement_tour_finding_members_no_update
                 BEFORE UPDATE ON engagement_tour_finding_members
                 BEGIN
                     SELECT RAISE(ABORT, 'tour finding members are immutable');
                 END;
             CREATE TRIGGER engagement_tour_finding_sets_no_update
                 BEFORE UPDATE ON engagement_tour_finding_sets
                 BEGIN
                     SELECT RAISE(ABORT, 'tour finding sets are immutable');
                 END;
             CREATE TRIGGER engagement_validation_comparison_items_no_update
                 BEFORE UPDATE ON engagement_validation_comparison_items
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'validation comparison items are immutable'
                     );
                 END;
             CREATE TRIGGER engagement_validation_comparisons_no_update
                 BEFORE UPDATE ON engagement_validation_comparisons
                 BEGIN
                     SELECT RAISE(ABORT, 'validation comparisons are immutable');
                 END;",
        )
        .expect("restore coherent member tamper guards");
    drop(connection);

    let report = integrity_check(fixture.temporary.path()).expect("tampered integrity report");
    assert!(!report.ok, "coherent member tamper escaped replay");
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.contains("differs from independent finding-set replay")),
        "{:?}",
        report.issues
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn coherent_comparison_item_relink_is_rejected_with_category_totals_unchanged() {
    let fixture = complete_tour_one();
    complete_tour_two(&fixture, "tour-2-coherent-item");
    let baseline = integrity_check(fixture.temporary.path()).expect("clean integrity baseline");
    assert!(baseline.ok, "{:?}", baseline.issues);
    let database_path = fixture.temporary.path().join("state.sqlite3");
    let connection = Connection::open(&database_path).expect("database");
    let before_counts = (
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_validation_comparison_items
             WHERE category = 'CLOSED'",
        ),
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_validation_comparison_items
             WHERE category = 'REMAINING'",
        ),
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_validation_comparison_items
             WHERE category = 'NEW'",
        ),
    );
    assert_eq!(before_counts, (4, 1, 1));
    let item = connection
        .query_row(
            "SELECT comparison_id, category, finding_fingerprint,
                    category_occurrence_index,
                    baseline_finding_set_id, baseline_occurrence_index,
                    baseline_member_hash, revised_finding_set_id,
                    revised_occurrence_index, revised_member_hash,
                    explanation_code
             FROM engagement_validation_comparison_items
             WHERE category = 'REMAINING'
             LIMIT 1",
            [],
            stored_comparison_item,
        )
        .expect("remaining comparison item");
    assert_ne!(item.baseline_set_id, item.revised_set_id);
    let swapped = StoredComparisonItem {
        baseline_set_id: item.revised_set_id.clone(),
        baseline_occurrence: item.revised_occurrence,
        baseline_member_hash: item.revised_member_hash.clone(),
        revised_set_id: item.baseline_set_id.clone(),
        revised_occurrence: item.baseline_occurrence,
        revised_member_hash: item.baseline_member_hash.clone(),
        ..item.clone()
    };
    let swapped_json = canonical_comparison_item(&swapped);
    let swapped_hash = test_sha256(swapped_json.as_bytes());
    connection
        .execute_batch("DROP TRIGGER engagement_validation_comparison_items_no_update;")
        .expect("open comparison relink window");
    assert_eq!(
        connection
            .execute(
                "UPDATE engagement_validation_comparison_items
                 SET baseline_finding_set_id = ?1,
                     baseline_occurrence_index = ?2,
                     baseline_member_hash = ?3,
                     revised_finding_set_id = ?4,
                     revised_occurrence_index = ?5,
                     revised_member_hash = ?6,
                     canonical_item_json = ?7,
                     item_hash = ?8
                 WHERE comparison_id = ?9 AND category = ?10
                   AND finding_fingerprint = ?11
                   AND category_occurrence_index = ?12",
                params![
                    swapped.baseline_set_id,
                    swapped.baseline_occurrence,
                    swapped.baseline_member_hash,
                    swapped.revised_set_id,
                    swapped.revised_occurrence,
                    swapped.revised_member_hash,
                    swapped_json,
                    swapped_hash,
                    swapped.comparison_id,
                    swapped.category,
                    swapped.fingerprint,
                    swapped.category_index,
                ],
            )
            .expect("coherently relink comparison item"),
        1
    );
    connection
        .execute_batch(
            "CREATE TRIGGER engagement_validation_comparison_items_no_update
                 BEFORE UPDATE ON engagement_validation_comparison_items
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'validation comparison items are immutable'
                     );
                 END;",
        )
        .expect("restore comparison-item guard");
    let after_counts = (
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_validation_comparison_items
             WHERE category = 'CLOSED'",
        ),
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_validation_comparison_items
             WHERE category = 'REMAINING'",
        ),
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_validation_comparison_items
             WHERE category = 'NEW'",
        ),
    );
    assert_eq!(after_counts, before_counts);
    drop(connection);

    let report = integrity_check(fixture.temporary.path()).expect("tampered integrity report");
    assert!(!report.ok, "coherent comparison relink escaped replay");
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.contains("differs from canonical comparison")),
        "{:?}",
        report.issues
    );
}

#[test]
fn same_created_at_tour_commands_replay_in_generation_order() {
    let fixture = complete_tour_one();
    complete_tour_two(&fixture, "tour-2-same-time-commands");
    let baseline = integrity_check(fixture.temporary.path()).expect("clean integrity baseline");
    assert!(baseline.ok, "{:?}", baseline.issues);
    let database_path = fixture.temporary.path().join("state.sqlite3");
    let connection = Connection::open(&database_path).expect("database");
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(*) FROM engagement_tour_command_attempts"
        ),
        4
    );
    connection
        .execute_batch(
            "DROP TRIGGER engagement_tour_command_attempts_no_update;
             UPDATE engagement_tour_command_attempts
             SET created_at = '2026-07-29T00:00:00.000Z',
                 attempt_id = printf(
                     'tour-command-same-time-%020d',
                     999999999999 - expected_generation
                 );
             CREATE TRIGGER engagement_tour_command_attempts_no_update
                 BEFORE UPDATE ON engagement_tour_command_attempts
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'engagement tour command attempts are immutable'
                     );
                 END;",
        )
        .expect("make command timestamps identical behind restored guard");
    assert_eq!(
        count(
            &connection,
            "SELECT COUNT(DISTINCT created_at)
             FROM engagement_tour_command_attempts"
        ),
        1
    );
    let lexicographic_generations = connection
        .prepare(
            "SELECT expected_generation
             FROM engagement_tour_command_attempts
             ORDER BY attempt_id",
        )
        .expect("command-order statement")
        .query_map([], |row| row.get::<_, i64>(0))
        .expect("command-order rows")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect command order");
    assert!(
        lexicographic_generations
            .windows(2)
            .all(|pair| pair[0] > pair[1]),
        "test setup must make UUID-style tie-break order oppose generation order: \
         {lexicographic_generations:?}"
    );
    drop(connection);

    let report = integrity_check(fixture.temporary.path()).expect("same-time integrity replay");
    assert!(report.ok, "{:?}", report.issues);
    assert_eq!(report.checked_engagement_tour_command_attempts, 4);

    let connection =
        Connection::open(fixture.temporary.path().join("state.sqlite3")).expect("database");
    coherently_shift_tour_command_generations(&connection, 10);
    drop(connection);
    let shifted =
        integrity_check(fixture.temporary.path()).expect("shifted-generation integrity replay");
    assert!(
        !shifted.ok,
        "coherent command-generation shift escaped replay"
    );
    assert!(
        shifted.issues.iter().any(|issue| issue
            .contains("initial START_TOUR generation differs from active-context authority")),
        "{:?}",
        shifted.issues
    );
    assert!(
        !shifted
            .issues
            .iter()
            .any(|issue| issue.contains("canonical/generation chain failed replay")),
        "test setup did not preserve the internal command chain: {:?}",
        shifted.issues
    );
    let read_error = fixture
        .service
        .tour_chain()
        .expect_err("comparison read must reject a coherently shifted command chain");
    assert!(
        read_error
            .to_string()
            .contains("initial START_TOUR generation differs from active-context authority"),
        "{read_error}"
    );
}

#[test]
fn global_integrity_rejects_deleted_start_tour_command_prefix() {
    let fixture = complete_tour_one();
    complete_tour_two(&fixture, "tour-2-deleted-start");
    let baseline = integrity_check(fixture.temporary.path()).expect("clean integrity baseline");
    assert!(baseline.ok, "{:?}", baseline.issues);

    let connection =
        Connection::open(fixture.temporary.path().join("state.sqlite3")).expect("database");
    assert_eq!(
        delete_tour_commands(
            &connection,
            "DELETE FROM engagement_tour_command_attempts
             WHERE command_kind = 'START_TOUR'",
        ),
        1
    );
    drop(connection);

    let report =
        integrity_check(fixture.temporary.path()).expect("deleted-prefix integrity report");
    assert!(!report.ok, "deleted START_TOUR escaped replay");
    assert_eq!(report.checked_engagement_tour_command_attempts, 3);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.contains("command history does not begin with START_TOUR")),
        "{:?}",
        report.issues
    );
    let read_error = fixture
        .service
        .tour_chain()
        .expect_err("comparison read must reject a deleted command prefix");
    assert!(
        read_error
            .to_string()
            .contains("command history does not begin with START_TOUR"),
        "{read_error}"
    );
}

#[test]
fn global_integrity_rejects_deleted_complete_tour_command_history() {
    let fixture = complete_tour_one();
    complete_tour_two(&fixture, "tour-2-deleted-history");
    let baseline = integrity_check(fixture.temporary.path()).expect("clean integrity baseline");
    assert!(baseline.ok, "{:?}", baseline.issues);

    let connection =
        Connection::open(fixture.temporary.path().join("state.sqlite3")).expect("database");
    assert_eq!(
        delete_tour_commands(&connection, "DELETE FROM engagement_tour_command_attempts",),
        4
    );
    drop(connection);

    let report =
        integrity_check(fixture.temporary.path()).expect("deleted-history integrity report");
    assert!(!report.ok, "deleted command history escaped replay");
    assert_eq!(report.checked_engagement_tour_command_attempts, 0);
    assert!(
        report
            .issues
            .iter()
            .any(|issue| issue.contains("tour head exists without its immutable command history")),
        "{:?}",
        report.issues
    );
    let read_error = fixture
        .service
        .tour_chain()
        .expect_err("comparison read must reject absent command history");
    assert!(
        read_error
            .to_string()
            .contains("tour head exists without its immutable command history"),
        "{read_error}"
    );
}

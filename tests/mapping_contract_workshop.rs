use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Write as _,
    path::{Path, PathBuf},
};

use migration_stream::{
    KernelError,
    destination::{
        APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA, ApproveDestinationProfileCommand,
        CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA, CreateManualCatalogueCommand,
        DESTINATION_FREEZE_ACKNOWLEDGEMENT, DestinationDraftSnapshot, DestinationFieldState,
        DestinationRuleDraft, FREEZE_DRAFT_COMMAND_SCHEMA, FreezeDestinationDraftCommand,
        LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT, ManualEntityDefinition,
        ManualPropertyDefinition, SAVE_DRAFT_COMMAND_SCHEMA, START_DRAFT_COMMAND_SCHEMA,
        SaveDestinationDraftCommand, StartDestinationDraftCommand,
    },
    engagement::{
        ACCEPT_SOURCE_COMMAND_SCHEMA, APPROVE_MAPPING_VERSION_COMMAND_SCHEMA,
        AcceptPreflightCommand, ApproveMappingVersionCommand, CREATE_ENGAGEMENT_COMMAND_SCHEMA,
        CompletePreflightCommand, ControlledObjectPack, CreateOrReopenCommand, EngagementPhase,
        EngagementService, EngagementServiceConfig, LAUNCH_VALIDATION_COMMAND_SCHEMA,
        LaunchValidationCommand, MappingContractCoverageStatus, MappingDraftSnapshot, MappingMode,
        OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT, OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT,
        RESERVE_PREFLIGHT_COMMAND_SCHEMA, ReservePreflightCommand,
        SAVE_MAPPING_DRAFT_COMMAND_SCHEMA, SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT,
        SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA, SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID,
        SaveMappingDraftCommand, SubmitMappingDraftCommand,
    },
    mapping::{MappingBinding, MappingKind, ReferenceTable, SourceEntityAssignment},
    object_workspace::establish_for_local_automation,
    policy::IntakePolicy,
    profiling::ProfilingPolicy,
    read_model::{IssueFilters, PageRequest, ReadService},
    state::integrity_check,
    validation::ValidationPolicy,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tempfile::TempDir;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const PROFILE_ID: &str = "workspace.mapping-contract.proof";
const PROFILE_VERSION: &str = "1.0.0";
const MAPPING_ID: &str = "workspace.mapping-contract.mapping";
const MAPPING_VERSION: &str = "1.0.0";

fn repository_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn configured_pack() -> ControlledObjectPack {
    ControlledObjectPack {
        object_pack_id: SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID.to_owned(),
        version: "1.0.0".to_owned(),
        title: "Synthetic compatibility pack".to_owned(),
        description: "Configured only to preserve the existing setup catalogue.".to_owned(),
        scope: "Synthetic local mapping and validation proof only.".to_owned(),
        root_entity_id: "SalesDocument".to_owned(),
        mapping_mode: MappingMode::OperatorGoverned,
        limitations: vec!["No target-system compatibility claim.".to_owned()],
        mapping_suggestions: Vec::new(),
        reference_tables: vec![
            ReferenceTable {
                table_id: "sales-document-type-domain".to_owned(),
                version: "1.0.0".to_owned(),
                entity_id: Some("SalesDocument".to_owned()),
                values: BTreeMap::from([
                    ("RETURN".to_owned(), "RETURN".to_owned()),
                    ("STANDARD".to_owned(), "STANDARD".to_owned()),
                ]),
            },
            ReferenceTable {
                table_id: "sales-document-route-map".to_owned(),
                version: "1.0.0".to_owned(),
                entity_id: Some("SalesDocument".to_owned()),
                values: BTreeMap::from([
                    ("N".to_owned(), "STANDARD".to_owned()),
                    ("R".to_owned(), "RETURN".to_owned()),
                ]),
            },
        ],
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
        object_packs: vec![configured_pack()],
    })
    .expect("open engagement service")
}

fn create_workspace_and_engagement(workspace: &Path) -> EngagementService {
    establish_for_local_automation(
        workspace,
        "Mapping contract workshop proof",
        "Root-only customer record",
        "Mapping contract integration test",
    )
    .expect("establish object workspace");
    let service = open_service(workspace);
    let created = service
        .create_or_reopen(&CreateOrReopenCommand {
            schema_version: CREATE_ENGAGEMENT_COMMAND_SCHEMA.to_owned(),
            display_name: "Mapping contract workshop proof".to_owned(),
            idempotency_key: "mapping-contract-create-engagement-0001".to_owned(),
        })
        .expect("create engagement");
    assert!(!created.reopened);
    service
}

fn reopen_engagement(workspace: &Path, suffix: &str) -> EngagementService {
    let service = open_service(workspace);
    let reopened = service
        .create_or_reopen(&CreateOrReopenCommand {
            schema_version: CREATE_ENGAGEMENT_COMMAND_SCHEMA.to_owned(),
            display_name: "Mapping contract workshop proof".to_owned(),
            idempotency_key: format!("mapping-contract-reopen-engagement-{suffix}"),
        })
        .expect("reopen engagement");
    assert!(reopened.reopened);
    service
}

fn rule(
    rule_id: &str,
    kind: &str,
    target: &str,
    parameters: Value,
    message: &str,
) -> DestinationRuleDraft {
    DestinationRuleDraft {
        rule_id: rule_id.to_owned(),
        severity: "error".to_owned(),
        kind: kind.to_owned(),
        target: target.to_owned(),
        message: message.to_owned(),
        parameters,
    }
}

#[allow(clippy::too_many_lines)]
fn approve_root_only_destination(service: &EngagementService) -> String {
    let destination = service.destination_service();
    let catalogue = destination
        .create_manual_catalogue(&CreateManualCatalogueCommand {
            schema_version: CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            artifact_label: "Root-only mapping contract destination".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Author a bounded single-entity destination for the mapping workshop proof."
                .to_owned(),
            entities: vec![ManualEntityDefinition {
                namespace: "MappingContract".to_owned(),
                name: "Record".to_owned(),
                properties: vec![
                    ManualPropertyDefinition {
                        name: "RecordId".to_owned(),
                        edm_type: "Edm.String".to_owned(),
                        nullable: false,
                        max_length: Some(64),
                        key_candidate: true,
                    },
                    ManualPropertyDefinition {
                        name: "Status".to_owned(),
                        edm_type: "Edm.String".to_owned(),
                        nullable: false,
                        max_length: Some(32),
                        key_candidate: false,
                    },
                    ManualPropertyDefinition {
                        name: "SourceStatus".to_owned(),
                        edm_type: "Edm.String".to_owned(),
                        nullable: false,
                        max_length: Some(32),
                        key_candidate: false,
                    },
                    ManualPropertyDefinition {
                        name: "DisplayName".to_owned(),
                        edm_type: "Edm.String".to_owned(),
                        nullable: false,
                        max_length: Some(128),
                        key_candidate: false,
                    },
                    ManualPropertyDefinition {
                        name: "Country".to_owned(),
                        edm_type: "Edm.String".to_owned(),
                        nullable: false,
                        max_length: Some(64),
                        key_candidate: false,
                    },
                    ManualPropertyDefinition {
                        name: "Tenant".to_owned(),
                        edm_type: "Edm.String".to_owned(),
                        nullable: false,
                        max_length: Some(32),
                        key_candidate: false,
                    },
                ],
            }],
            idempotency_key: "mapping-contract-create-manual-catalogue-0001".to_owned(),
        })
        .expect("create manual catalogue");
    let draft = destination
        .start_draft(&StartDestinationDraftCommand {
            schema_version: START_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            catalogue_authority_id: catalogue.catalogue_authority_id,
            expected_catalogue_hash: catalogue.catalogue_hash,
            root_technical_entity: "MappingContract.Record".to_owned(),
            child_technical_entities: Vec::new(),
            profile_id: PROFILE_ID.to_owned(),
            title: "Root-only mapping contract".to_owned(),
            owner: "Destination practitioner".to_owned(),
            actor: "Destination practitioner".to_owned(),
            reason: "Select the exact single-entity destination scope.".to_owned(),
            idempotency_key: "mapping-contract-start-destination-draft-0001".to_owned(),
        })
        .expect("start destination draft");
    assert_eq!(draft.snapshot.entities.len(), 1);
    assert_eq!(
        draft.snapshot.relationships,
        [] as [migration_stream::destination::DestinationRelationshipDraft; 0]
    );
    let entity_id = draft.snapshot.entities[0].entity_id.clone();
    let mut snapshot: DestinationDraftSnapshot = draft.snapshot;
    for field in &mut snapshot.entities[0].fields {
        field.state = DestinationFieldState::Active;
        field.business_required = field.field_id == "recordId";
    }
    snapshot.rules = vec![
        rule(
            "required-record-id",
            "required",
            &format!("{entity_id}.recordId"),
            json!({}),
            "A stable record identifier is required.",
        ),
        rule(
            "conditional-display-name",
            "conditional_required",
            &format!("{entity_id}.displayName"),
            json!({
                "whenField": format!("{entity_id}.status"),
                "equals": "ACTIVE"
            }),
            "An active record requires a display name.",
        ),
        rule(
            "reference-country",
            "reference_lookup",
            &format!("{entity_id}.country"),
            json!({
                "tableId": "country-domain",
                "tableVersion": "1.0.0"
            }),
            "Country must exist in the exact reviewed local domain.",
        ),
    ];
    let saved = destination
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: draft.draft_id,
            expected_draft_generation: draft.generation,
            expected_draft_hash: draft.draft_hash,
            snapshot,
            actor: "Destination practitioner".to_owned(),
            reason: "Confirm all active destination fields and bounded rules.".to_owned(),
            idempotency_key: "mapping-contract-save-destination-draft-0001".to_owned(),
        })
        .expect("save destination draft");
    assert!(
        saved.freeze_blockers.is_empty(),
        "{:?}",
        saved.freeze_blockers
    );
    let frozen = destination
        .freeze_draft(&FreezeDestinationDraftCommand {
            schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            draft_id: saved.draft_id,
            expected_draft_generation: saved.generation,
            expected_draft_hash: saved.draft_hash,
            profile_version: PROFILE_VERSION.to_owned(),
            acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
            actor: "Destination reviewer".to_owned(),
            reason: "Freeze the exact root-only destination contract.".to_owned(),
            idempotency_key: "mapping-contract-freeze-destination-0001".to_owned(),
        })
        .expect("freeze destination profile");
    assert_eq!(frozen.profile.declared_status, "REVIEWED_DRAFT");
    destination
        .approve_destination_profile(&ApproveDestinationProfileCommand {
            schema_version: APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: service.state().expect("state").generation,
            profile_id: frozen.profile.profile_id,
            profile_version: frozen.profile.profile_version,
            profile_hash: frozen.profile.profile_hash,
            origin_binding_hash: frozen.origin_binding_hash,
            actor: "Destination approver".to_owned(),
            reason: "Approve this exact profile only for local mapping and validation.".to_owned(),
            authority_acknowledgement: LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT.to_owned(),
            idempotency_key: "mapping-contract-approve-destination-0001".to_owned(),
        })
        .expect("approve destination profile");
    entity_id
}

fn write_source_zip(path: &Path) {
    let file = File::create(path).expect("create source ZIP");
    let mut writer = ZipWriter::new(file);
    writer
        .start_file(
            "unfamiliar_records.csv",
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .expect("start CSV");
    writer
        .write_all(
            b"record_ref,state_code,person_label,country_code\n\
              REC-001,ACTIVATE,   ,COUNTRY-DE\n\
              REC-002,IDLE,Bob,COUNTRY-UNKNOWN-PRIVATE\n",
        )
        .expect("write CSV");
    writer.finish().expect("finish source ZIP");
}

fn file_identity(path: &Path) -> (String, u64) {
    let bytes = fs::read(path).expect("read source ZIP");
    (
        migration_stream::canonical::lower_hex(Sha256::digest(&bytes)),
        u64::try_from(bytes.len()).expect("source size"),
    )
}

fn accept_sample_free_source(service: &EngagementService, workspace: &Path) -> (String, String) {
    let reservation = service
        .reserve_preflight(&ReservePreflightCommand {
            schema_version: RESERVE_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            source_name: "unfamiliar-records.zip".to_owned(),
            idempotency_key: "mapping-contract-reserve-source-0001".to_owned(),
        })
        .expect("reserve source");
    let source_zip = workspace.join("mapping-contract-source.zip");
    write_source_zip(&source_zip);
    fs::copy(&source_zip, &reservation.controlled_path).expect("stream source ZIP");
    let (source_hash, source_size) = file_identity(&source_zip);
    let preflight = service
        .complete_preflight(&CompletePreflightCommand {
            preflight_id: reservation.preflight.preflight_id,
            sha256: source_hash.clone(),
            size_bytes: source_size,
        })
        .expect("complete source preflight");
    let accepted = service
        .accept_preflight(&AcceptPreflightCommand {
            schema_version: ACCEPT_SOURCE_COMMAND_SCHEMA.to_owned(),
            preflight_id: preflight.preflight_id,
            expected_source_hash: source_hash,
            expected_manifest_hash: preflight.manifest_hash.expect("manifest hash"),
            idempotency_key: "mapping-contract-accept-source-0001".to_owned(),
            acknowledgement: SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("accept sample-free source");
    let profiling = ReadService::new(workspace)
        .profiling_summary(
            &accepted.profiling_run_id,
            true,
            &PageRequest {
                page_size: 200,
                cursor: None,
            },
        )
        .expect("read profiling evidence");
    assert!(profiling.include_samples);
    assert!(
        profiling
            .items
            .iter()
            .all(|item| item.sample_count == 0 && item.samples.as_deref() == Some(&[]))
    );
    (accepted.job_id, accepted.profiling_run_id)
}

fn source_binding(
    binding_id: &str,
    entity_id: &str,
    source_column: &str,
    source_column_ordinal: usize,
    target_field_id: &str,
    kind: MappingKind,
    lookup_table_id: Option<&str>,
) -> MappingBinding {
    MappingBinding {
        binding_id: binding_id.to_owned(),
        target_entity_id: Some(entity_id.to_owned()),
        source_file: Some("unfamiliar_records.csv".to_owned()),
        source_column: Some(source_column.to_owned()),
        source_column_ordinal: Some(source_column_ordinal),
        target_field_id: target_field_id.to_owned(),
        kind,
        constant: None,
        lookup_table_id: lookup_table_id.map(str::to_owned),
    }
}

fn governed_mapping_snapshot(entity_id: &str) -> MappingDraftSnapshot {
    MappingDraftSnapshot {
        source_entity_assignments: vec![SourceEntityAssignment {
            source_file: "unfamiliar_records.csv".to_owned(),
            entity_id: entity_id.to_owned(),
        }],
        bindings: vec![
            source_binding(
                "record-id",
                entity_id,
                "record_ref",
                0,
                "recordId",
                MappingKind::Direct,
                None,
            ),
            source_binding(
                "reviewed-status",
                entity_id,
                "state_code",
                1,
                "status",
                MappingKind::Lookup,
                Some("status-map"),
            ),
            source_binding(
                "received-status",
                entity_id,
                "state_code",
                1,
                "sourceStatus",
                MappingKind::Direct,
                None,
            ),
            source_binding(
                "display-name",
                entity_id,
                "person_label",
                2,
                "displayName",
                MappingKind::Trim,
                None,
            ),
            source_binding(
                "country",
                entity_id,
                "country_code",
                3,
                "country",
                MappingKind::Direct,
                None,
            ),
            MappingBinding {
                binding_id: "fixed-tenant".to_owned(),
                target_entity_id: Some(entity_id.to_owned()),
                source_file: None,
                source_column: None,
                source_column_ordinal: None,
                target_field_id: "tenant".to_owned(),
                kind: MappingKind::Constant,
                constant: Some("LOCAL-PROOF-TENANT-PRIVATE".to_owned()),
                lookup_table_id: None,
            },
        ],
        relationship_bindings: Vec::new(),
        reference_tables: vec![
            ReferenceTable {
                table_id: "country-domain".to_owned(),
                version: "1.0.0".to_owned(),
                entity_id: Some(entity_id.to_owned()),
                values: BTreeMap::from([
                    (
                        "COUNTRY-DE".to_owned(),
                        "Germany private reference label".to_owned(),
                    ),
                    (
                        "COUNTRY-US".to_owned(),
                        "United States private reference label".to_owned(),
                    ),
                ]),
            },
            ReferenceTable {
                table_id: "status-map".to_owned(),
                version: "1.0.0".to_owned(),
                entity_id: Some(entity_id.to_owned()),
                values: BTreeMap::from([
                    ("ACTIVATE".to_owned(), "ACTIVE".to_owned()),
                    ("IDLE".to_owned(), "INACTIVE".to_owned()),
                ]),
            },
        ],
        author: Some("Mapping practitioner".to_owned()),
    }
}

fn assert_invalid_table_snapshot(
    service: &EngagementService,
    snapshot: MappingDraftSnapshot,
    suffix: &str,
) {
    let workspace = service.mapping_workspace().expect("mapping workspace");
    assert!(workspace.draft.is_none());
    let error = service
        .save_mapping_draft(&SaveMappingDraftCommand {
            schema_version: SAVE_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: format!("mapping-contract-invalid-table-{suffix}"),
            expected_engagement_generation: workspace.engagement_generation,
            expected_draft_id: None,
            expected_draft_generation: None,
            expected_draft_hash: None,
            snapshot,
        })
        .expect_err("invalid reference table must fail before draft creation");
    assert!(
        matches!(
            error,
            KernelError::MappingInvalid(_)
                | KernelError::PolicyInvalid(_)
                | KernelError::ProfileSemantic(_)
        ),
        "unexpected error: {error:?}"
    );
    assert!(
        service
            .mapping_workspace()
            .expect("mapping workspace after rejected save")
            .draft
            .is_none()
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn root_only_mapping_contract_is_durable_value_free_and_fail_closed() {
    let temporary = TempDir::new().expect("temporary mapping-contract workspace");
    let workspace = temporary.path();
    let service = create_workspace_and_engagement(workspace);
    let entity_id = approve_root_only_destination(&service);
    let (job_id, profiling_run_id) = accept_sample_free_source(&service, workspace);

    let mapping_workspace = service.mapping_workspace().expect("mapping workspace");
    assert_eq!(mapping_workspace.destination_origin, "WORKSPACE_PROFILE");
    assert_eq!(mapping_workspace.job_id, job_id);
    assert_eq!(mapping_workspace.profile_id, PROFILE_ID);
    assert_eq!(mapping_workspace.profile_version, PROFILE_VERSION);
    assert_eq!(mapping_workspace.entities.len(), 1);
    assert_eq!(
        mapping_workspace.relationships,
        [] as [migration_stream::engagement::MappingWorkspaceRelationshipView; 0]
    );
    assert_eq!(mapping_workspace.reference_table_obligations.len(), 1);
    assert_eq!(
        mapping_workspace.reference_table_obligations[0].table_id,
        "country-domain"
    );

    let valid = governed_mapping_snapshot(&entity_id);

    let mut missing = valid.clone();
    missing
        .reference_tables
        .retain(|table| table.table_id != "country-domain");
    assert_invalid_table_snapshot(&service, missing, "missing-0001");

    let mut empty = valid.clone();
    empty
        .reference_tables
        .iter_mut()
        .find(|table| table.table_id == "country-domain")
        .expect("country table")
        .values
        .clear();
    assert_invalid_table_snapshot(&service, empty, "empty-0001");

    let mut wrong_version = valid.clone();
    wrong_version
        .reference_tables
        .iter_mut()
        .find(|table| table.table_id == "country-domain")
        .expect("country table")
        .version = "2.0.0".to_owned();
    assert_invalid_table_snapshot(&service, wrong_version, "wrong-version-0001");

    let mut wrong_entity = valid.clone();
    wrong_entity
        .reference_tables
        .iter_mut()
        .find(|table| table.table_id == "country-domain")
        .expect("country table")
        .entity_id = Some("otherEntity".to_owned());
    assert_invalid_table_snapshot(&service, wrong_entity, "wrong-entity-0001");

    let mut duplicate = valid.clone();
    duplicate
        .reference_tables
        .push(duplicate.reference_tables[0].clone());
    assert_invalid_table_snapshot(&service, duplicate, "duplicate-0001");

    let mut oversized = valid.clone();
    oversized
        .reference_tables
        .iter_mut()
        .find(|table| table.table_id == "country-domain")
        .expect("country table")
        .values
        .insert("COUNTRY-OVERSIZE".to_owned(), "x".repeat(4_097));
    assert_invalid_table_snapshot(&service, oversized, "oversize-0001");

    let saved = service
        .save_mapping_draft(&SaveMappingDraftCommand {
            schema_version: SAVE_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "mapping-contract-save-draft-0001".to_owned(),
            expected_engagement_generation: mapping_workspace.engagement_generation,
            expected_draft_id: None,
            expected_draft_generation: None,
            expected_draft_hash: None,
            snapshot: valid.clone(),
        })
        .expect("save exact mapping draft");
    assert_eq!(
        saved.draft.snapshot.relationship_bindings,
        [] as [migration_stream::mapping::MappingRelationshipBinding; 0]
    );
    let status_bindings = saved
        .draft
        .snapshot
        .bindings
        .iter()
        .filter(|binding| {
            binding.source_file.as_deref() == Some("unfamiliar_records.csv")
                && binding.source_column.as_deref() == Some("state_code")
                && binding.source_column_ordinal == Some(1)
        })
        .collect::<Vec<_>>();
    assert_eq!(status_bindings.len(), 2);
    assert_ne!(
        status_bindings[0].target_field_id,
        status_bindings[1].target_field_id
    );

    let coverage = service
        .mapping_contract_coverage()
        .expect("derive value-free contract coverage");
    assert_eq!(
        coverage.overall_status,
        MappingContractCoverageStatus::Complete
    );
    assert_eq!(coverage.summary.blockers, 0);
    assert_eq!(coverage.summary.unresolved, 0);
    assert_eq!(
        coverage.relationships,
        [] as [migration_stream::engagement::MappingContractRelationshipCoverage; 0]
    );
    assert_eq!(coverage.authority.job_id, job_id);
    assert_eq!(coverage.authority.profiling_run_id, profiling_run_id);
    assert_eq!(
        coverage
            .fields
            .iter()
            .filter(|field| field.source_column.as_deref() == Some("state_code"))
            .count(),
        2
    );
    assert!(
        coverage
            .fields
            .iter()
            .find(|field| field.field_id == "tenant")
            .is_some_and(|field| {
                field.operation == Some(MappingKind::Constant)
                    && field.fixed_value_configured
                    && field.source_file.is_none()
                    && field.source_column.is_none()
            })
    );
    let coverage_json = serde_json::to_string(&coverage).expect("serialize coverage");
    for private_value in [
        "ACTIVATE",
        "ACTIVE",
        "COUNTRY-DE",
        "COUNTRY-UNKNOWN-PRIVATE",
        "Germany private reference label",
        "LOCAL-PROOF-TENANT-PRIVATE",
    ] {
        assert!(
            !coverage_json.contains(private_value),
            "coverage leaked {private_value:?}"
        );
    }

    drop(service);
    let reopened = reopen_engagement(workspace, "after-save-0001");
    let reopened_workspace = reopened
        .mapping_workspace()
        .expect("reopen exact mapping workspace");
    assert_eq!(reopened_workspace.draft.as_ref(), Some(&saved.draft));
    let reopened_coverage = reopened
        .mapping_contract_coverage()
        .expect("reopen exact coverage");
    assert_eq!(reopened_coverage.coverage_hash, coverage.coverage_hash);

    let submitted = reopened
        .submit_mapping_draft(&SubmitMappingDraftCommand {
            schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "mapping-contract-submit-draft-0001".to_owned(),
            expected_engagement_generation: reopened_workspace.engagement_generation,
            draft_id: saved.draft.draft_id.clone(),
            expected_draft_generation: saved.draft.draft_generation,
            expected_draft_hash: saved.draft.draft_hash.clone(),
            mapping_id: MAPPING_ID.to_owned(),
            version: MAPPING_VERSION.to_owned(),
            acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("freeze exact mapping");
    let submitted_workspace = reopened
        .mapping_workspace()
        .expect("reload submitted mapping");
    let submitted_draft = submitted_workspace.draft.expect("submitted draft");
    let approval = reopened
        .approve_mapping_version(&ApproveMappingVersionCommand {
            schema_version: APPROVE_MAPPING_VERSION_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "mapping-contract-approve-mapping-0001".to_owned(),
            expected_engagement_generation: submitted_workspace.engagement_generation,
            draft_id: submitted_draft.draft_id,
            expected_draft_generation: submitted_draft.draft_generation,
            expected_draft_hash: submitted_draft.draft_hash,
            expected_mapping_id: MAPPING_ID.to_owned(),
            expected_mapping_version: MAPPING_VERSION.to_owned(),
            expected_mapping_hash: submitted.mapping_hash,
            actor: "Mapping approver".to_owned(),
            reason: "Approve this exact mapping and embedded tables for local validation."
                .to_owned(),
            acknowledgement: OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("approve exact mapping hash");

    drop(reopened);
    let validation_service = reopen_engagement(workspace, "before-validation-0001");
    let validation = validation_service
        .launch_validation(&LaunchValidationCommand {
            schema_version: LAUNCH_VALIDATION_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "mapping-contract-launch-validation-0001".to_owned(),
            expected_engagement_generation: validation_service
                .state()
                .expect("validation state")
                .generation,
            expected_job_id: job_id,
            expected_mapping_id: approval.mapping_id,
            expected_mapping_version: approval.mapping_version,
            expected_mapping_hash: approval.mapping_hash,
        })
        .expect("launch exact approved mapping validation");
    assert_eq!(validation.status.rows_processed, 2);
    assert_eq!(validation.status.issue_count, 2);
    assert_eq!(
        validation_service.state().expect("final state").phase,
        EngagementPhase::FirstIssuesReady
    );

    let issues = ReadService::new(workspace)
        .issue_list(
            &validation.status.run_id,
            &IssueFilters::default(),
            &PageRequest {
                page_size: 200,
                cursor: None,
            },
        )
        .expect("read value-suppressed issues");
    let issue_codes = issues
        .items
        .iter()
        .map(|issue| issue.issue_code.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        issue_codes,
        BTreeSet::from(["CONDITIONAL_REQUIRED_MISSING", "REFERENCE_LOOKUP_MISSING"])
    );
    let issues_json = serde_json::to_string(&issues).expect("serialize issues");
    for private_value in [
        "ACTIVATE",
        "COUNTRY-UNKNOWN-PRIVATE",
        "Germany private reference label",
        "LOCAL-PROOF-TENANT-PRIVATE",
    ] {
        assert!(
            !issues_json.contains(private_value),
            "issue page leaked {private_value:?}"
        );
    }

    let integrity = integrity_check(workspace).expect("global integrity replay");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

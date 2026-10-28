use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Barrier, mpsc},
    thread,
    time::Duration,
};

use fs2::FileExt as _;
use migration_stream::{
    KernelError,
    canonical::{canonical_hash, canonical_json_string},
    destination::{
        APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA, ApproveDestinationProfileCommand,
        CARRY_FORWARD_MAPPING_COMMAND_SCHEMA, CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA,
        CarryForwardMappingCommand, CreateManualCatalogueCommand,
        DESTINATION_FREEZE_ACKNOWLEDGEMENT, DestinationDraftSnapshot, DestinationEntityDraft,
        DestinationFieldDraft, DestinationFieldState, DestinationRelationshipDraft,
        DestinationRelationshipKeyPair, DestinationRuleDraft, DestinationService,
        FREEZE_DRAFT_COMMAND_SCHEMA, FreezeDestinationDraftCommand,
        LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT, ManualEntityDefinition,
        ManualPropertyDefinition, ManualSupplementEvidence, SAVE_DRAFT_COMMAND_SCHEMA,
        START_DRAFT_COMMAND_SCHEMA, SaveDestinationDraftCommand, StartDestinationDraftCommand,
    },
    engagement::{
        ACCEPT_SOURCE_COMMAND_SCHEMA, APPROVE_MAPPING_VERSION_COMMAND_SCHEMA,
        APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA, AcceptPreflightCommand,
        ApproveMappingVersionCommand, ApproveProfileCommand, CREATE_ENGAGEMENT_COMMAND_SCHEMA,
        CompletePreflightCommand, ControlledObjectPack, CreateOrReopenCommand, EngagementPhase,
        EngagementService, EngagementServiceConfig, LAUNCH_VALIDATION_COMMAND_SCHEMA,
        LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT, LaunchValidationCommand,
        MappingDraftSnapshot, MappingMode, OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT,
        OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT, RESERVE_PREFLIGHT_COMMAND_SCHEMA,
        ReservePreflightCommand, SAVE_MAPPING_DRAFT_COMMAND_SCHEMA,
        SELECT_OBJECT_PACK_COMMAND_SCHEMA, SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT,
        SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA, SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID,
        SaveMappingDraftCommand, SaveMappingDraftResult, SelectObjectPackCommand,
        SourcePreflightState, SubmitMappingDraftCommand,
    },
    mapping::{
        MappingBinding, MappingDocument, MappingKind, MappingProfileIdentity,
        MappingRelationshipBinding, MappingSourceIdentity, ReferenceTable, RelationshipFieldPair,
        SourceEntityAssignment,
    },
    policy::IntakePolicy,
    profiling::ProfilingPolicy,
    read_model::{IssueFilters, PageRequest, ReadService},
    state::integrity_check,
    validation::ValidationPolicy,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

const HEADER_ENTITY: &str = "SalesDocument";
const ITEM_ENTITY: &str = "SalesDocumentItem";
const RACE_TEST_TIMEOUT: Duration = Duration::from_secs(10);

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

fn sales_service(workspace: &Path) -> EngagementService {
    migration_stream::object_workspace::establish_for_local_automation(
        workspace,
        "Synthetic sales-document workspace",
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
        validation_policy: ValidationPolicy::from_path(&repository_path(
            "config/engagement-validation-policy.json",
        ))
        .expect("validation policy"),
        object_packs: vec![sales_pack()],
    })
    .expect("engagement service")
}

fn build_sales_zip(path: &Path) {
    let file = File::create(path).expect("create sales source zip");
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for source_file in ["deal_book.csv", "deal_detail.csv"] {
        writer
            .start_file(source_file, options)
            .expect("start source file");
        writer
            .write_all(
                &fs::read(repository_path(&format!(
                    "fixtures/sales-document-unfamiliar/{source_file}"
                )))
                .expect("read source fixture"),
            )
            .expect("write source fixture");
    }
    writer.finish().expect("finish sales source zip");
}

fn file_identity(path: &Path) -> (String, u64) {
    let bytes = fs::read(path).expect("source package bytes");
    (
        migration_stream::canonical::lower_hex(Sha256::digest(&bytes)),
        u64::try_from(bytes.len()).expect("source package size"),
    )
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
        author: Some("local-practitioner".to_owned()),
    }
}

#[allow(clippy::too_many_lines)]
fn carried_workspace_profile_snapshot() -> DestinationDraftSnapshot {
    let source_profile = sales_pack().profile_document;
    let entities = source_profile["entities"]
        .as_array()
        .expect("sales profile entities")
        .iter()
        .enumerate()
        .map(|(entity_ordinal, entity)| {
            let entity_id = entity["entityId"]
                .as_str()
                .expect("sales profile entity ID")
                .to_owned();
            let mut fields = entity["fields"]
                .as_array()
                .expect("sales profile fields")
                .iter()
                .enumerate()
                .map(|(field_ordinal, field)| DestinationFieldDraft {
                    field_id: field["fieldId"]
                        .as_str()
                        .expect("sales profile field ID")
                        .to_owned(),
                    label: field["label"]
                        .as_str()
                        .expect("sales profile field label")
                        .to_owned(),
                    logical_type: field["logicalType"]
                        .as_str()
                        .expect("sales profile logical type")
                        .to_owned(),
                    business_required: field["required"]
                        .as_bool()
                        .expect("sales profile required flag"),
                    sensitive: field["sensitive"]
                        .as_bool()
                        .expect("sales profile sensitivity flag"),
                    key: entity["key"]
                        .as_array()
                        .expect("sales profile entity key")
                        .iter()
                        .any(|candidate| candidate == &field["fieldId"]),
                    state: DestinationFieldState::Active,
                    technical_property: None,
                    manual_supplement: Some(ManualSupplementEvidence {
                        supplement_id: format!(
                            "carry-supplement-{entity_ordinal}-{field_ordinal}"
                        ),
                        actor: "destination-practitioner".to_owned(),
                        reason:
                            "Retain the reviewed target-neutral field as explicit manual destination evidence."
                                .to_owned(),
                    }),
                    technical_change_review: None,
                    confirm_max_length: false,
                    max_length: None,
                    diff_state: None,
                })
                .collect::<Vec<_>>();
            if entity_id == HEADER_ENTITY {
                fields.push(DestinationFieldDraft {
                    field_id: "deliveryNote".to_owned(),
                    label: "Delivery note".to_owned(),
                    logical_type: "text".to_owned(),
                    business_required: false,
                    sensitive: false,
                    key: false,
                    state: DestinationFieldState::Active,
                    technical_property: None,
                    manual_supplement: Some(ManualSupplementEvidence {
                        supplement_id: "carry-supplement-delivery-note".to_owned(),
                        actor: "destination-practitioner".to_owned(),
                        reason:
                            "Add one new optional destination field for explicit mapping review."
                                .to_owned(),
                    }),
                    technical_change_review: None,
                    confirm_max_length: false,
                    max_length: None,
                    diff_state: None,
                });
            }
            DestinationEntityDraft {
                entity_id: entity_id.clone(),
                label: entity_id,
                root: entity["root"]
                    .as_bool()
                    .expect("sales profile root flag"),
                technical_entity: None,
                fields,
            }
        })
        .collect::<Vec<_>>();
    let relationships = source_profile["relationships"]
        .as_array()
        .expect("sales profile relationships")
        .iter()
        .map(|relationship| DestinationRelationshipDraft {
            relationship_id: relationship["relationshipId"]
                .as_str()
                .expect("sales profile relationship ID")
                .to_owned(),
            from_entity_id: relationship["from"]
                .as_str()
                .expect("sales profile relationship source")
                .to_owned(),
            to_entity_id: relationship["to"]
                .as_str()
                .expect("sales profile relationship target")
                .to_owned(),
            cardinality: relationship["cardinality"]
                .as_str()
                .expect("sales profile relationship cardinality")
                .to_owned(),
            technical_navigation: None,
            key_pairs: relationship["keyPairs"]
                .as_array()
                .expect("sales profile relationship key pairs")
                .iter()
                .map(|pair| DestinationRelationshipKeyPair {
                    from_field_id: pair["fromFieldId"]
                        .as_str()
                        .expect("sales profile relationship source field")
                        .to_owned(),
                    to_field_id: pair["toFieldId"]
                        .as_str()
                        .expect("sales profile relationship target field")
                        .to_owned(),
                })
                .collect(),
            confirmed: true,
        })
        .collect();
    let rules = source_profile["rules"]
        .as_array()
        .expect("sales profile rules")
        .iter()
        .filter(|rule| rule["kind"] != "reference_lookup")
        .map(|rule| DestinationRuleDraft {
            rule_id: rule["ruleId"]
                .as_str()
                .expect("sales profile rule ID")
                .to_owned(),
            severity: rule["severity"]
                .as_str()
                .expect("sales profile rule severity")
                .to_owned(),
            kind: rule["kind"]
                .as_str()
                .expect("sales profile rule kind")
                .to_owned(),
            target: rule["target"]
                .as_str()
                .expect("sales profile rule target")
                .to_owned(),
            message: rule["message"]
                .as_str()
                .expect("sales profile rule message")
                .to_owned(),
            parameters: rule["parameters"].clone(),
        })
        .collect();
    DestinationDraftSnapshot {
        profile_id: SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID.to_owned(),
        title: "Synthetic basic sales document workspace profile".to_owned(),
        owner: "Destination practitioner".to_owned(),
        entities,
        relationships,
        rules,
        compatibility_acknowledgements: Vec::new(),
    }
}

#[allow(clippy::too_many_lines)]
fn prepare_validated_sales_mapping(workspace: &Path) -> EngagementService {
    let service = sales_service(workspace);
    service
        .create_or_reopen(&CreateOrReopenCommand {
            schema_version: CREATE_ENGAGEMENT_COMMAND_SCHEMA.to_owned(),
            display_name: "Sales mapping carry-forward".to_owned(),
            idempotency_key: "carry-create-0001".to_owned(),
        })
        .expect("create carry-forward engagement");
    let reservation = service
        .reserve_preflight(&ReservePreflightCommand {
            schema_version: RESERVE_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            source_name: "unfamiliar-sales-carry.zip".to_owned(),
            idempotency_key: "carry-reserve-0001".to_owned(),
        })
        .expect("reserve carry-forward source");
    let source_zip = workspace.join("sales-carry-source.zip");
    build_sales_zip(&source_zip);
    fs::copy(&source_zip, &reservation.controlled_path)
        .expect("stream carry-forward source package");
    let (source_hash, source_size) = file_identity(&source_zip);
    let preflight = service
        .complete_preflight(&CompletePreflightCommand {
            preflight_id: reservation.preflight.preflight_id,
            sha256: source_hash.clone(),
            size_bytes: source_size,
        })
        .expect("complete carry-forward preflight");
    let accepted = service
        .accept_preflight(&AcceptPreflightCommand {
            schema_version: ACCEPT_SOURCE_COMMAND_SCHEMA.to_owned(),
            preflight_id: preflight.preflight_id,
            expected_source_hash: source_hash,
            expected_manifest_hash: preflight.manifest_hash.expect("carry manifest hash"),
            idempotency_key: "carry-accept-0001".to_owned(),
            acknowledgement: SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("accept carry-forward source");
    service
        .select_object_pack(&SelectObjectPackCommand {
            schema_version: SELECT_OBJECT_PACK_COMMAND_SCHEMA.to_owned(),
            object_pack_id: SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID.to_owned(),
            idempotency_key: "carry-select-pack-0001".to_owned(),
        })
        .expect("select carry-forward source profile");
    service
        .approve_profile(&ApproveProfileCommand {
            schema_version: APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA.to_owned(),
            actor: "mapping-practitioner".to_owned(),
            reason: "Approve the exact controlled source profile for carry-forward validation."
                .to_owned(),
            authority_acknowledgement: LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT
                .to_owned(),
            idempotency_key: "carry-approve-profile-0001".to_owned(),
        })
        .expect("approve carry-forward source profile");
    let mapping_workspace = service
        .mapping_workspace()
        .expect("open carry-forward mapping workspace");
    let saved = service
        .save_mapping_draft(&SaveMappingDraftCommand {
            schema_version: SAVE_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "carry-save-mapping-0001".to_owned(),
            expected_engagement_generation: mapping_workspace.engagement_generation,
            expected_draft_id: None,
            expected_draft_generation: None,
            expected_draft_hash: None,
            snapshot: governed_snapshot(),
        })
        .expect("save carry-forward source mapping");
    let submitted = service
        .submit_mapping_draft(&SubmitMappingDraftCommand {
            schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "carry-submit-mapping-0001".to_owned(),
            expected_engagement_generation: saved.engagement_generation,
            draft_id: saved.draft.draft_id,
            expected_draft_generation: saved.draft.draft_generation,
            expected_draft_hash: saved.draft.draft_hash,
            mapping_id: "synthetic.sales-document.carry-source".to_owned(),
            version: "1.0.0".to_owned(),
            acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("submit carry-forward source mapping");
    let submitted_workspace = service
        .mapping_workspace()
        .expect("reload submitted carry-forward mapping");
    let submitted_draft = submitted_workspace.draft.expect("submitted carry draft");
    let approved = service
        .approve_mapping_version(&ApproveMappingVersionCommand {
            schema_version: APPROVE_MAPPING_VERSION_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "carry-approve-mapping-0001".to_owned(),
            expected_engagement_generation: submitted_workspace.engagement_generation,
            draft_id: submitted_draft.draft_id,
            expected_draft_generation: submitted_draft.draft_generation,
            expected_draft_hash: submitted_draft.draft_hash,
            expected_mapping_id: "synthetic.sales-document.carry-source".to_owned(),
            expected_mapping_version: "1.0.0".to_owned(),
            expected_mapping_hash: submitted.mapping_hash,
            actor: "mapping-practitioner".to_owned(),
            reason: "Approve the exact immutable source mapping for carry-forward.".to_owned(),
            acknowledgement: OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("approve carry-forward source mapping");
    service
        .launch_validation(&LaunchValidationCommand {
            schema_version: LAUNCH_VALIDATION_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "carry-launch-validation-0001".to_owned(),
            expected_engagement_generation: service.state().expect("validation state").generation,
            expected_job_id: accepted.job_id,
            expected_mapping_id: approved.mapping_id,
            expected_mapping_version: approved.mapping_version,
            expected_mapping_hash: approved.mapping_hash,
        })
        .expect("validate carry-forward source authority");
    service
}

#[test]
#[allow(clippy::too_many_lines)]
fn exact_approved_mapping_carry_forward_is_atomic_and_restart_durable() {
    let temporary = TempDir::new().expect("temporary carry-forward workspace");
    let engagement = prepare_validated_sales_mapping(temporary.path());
    let destination = DestinationService::new(
        temporary.path(),
        repository_path("schemas/migration-object-profile.schema.json"),
    );
    let source_workshop = destination
        .workshop()
        .expect("open validated source destination workshop");
    let eligible = source_workshop
        .eligible_carry_forward_source
        .clone()
        .expect("exact approved source mapping is eligible");
    assert_eq!(eligible.mapping_id, "synthetic.sales-document.carry-source");
    assert_eq!(eligible.mapping_version, "1.0.0");

    let manual_catalogue = destination
        .create_manual_catalogue(&CreateManualCatalogueCommand {
            schema_version: CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: source_workshop.engagement_generation,
            artifact_label: "Reviewed sales-document manual supplement".to_owned(),
            actor: "destination-practitioner".to_owned(),
            reason: "Create bounded technical anchors for a separately governed workspace profile."
                .to_owned(),
            entities: vec![
                ManualEntityDefinition {
                    namespace: "CarryForward".to_owned(),
                    name: "HeaderAnchor".to_owned(),
                    properties: vec![ManualPropertyDefinition {
                        name: "AnchorId".to_owned(),
                        edm_type: "Edm.String".to_owned(),
                        nullable: false,
                        max_length: None,
                        key_candidate: true,
                    }],
                },
                ManualEntityDefinition {
                    namespace: "CarryForward".to_owned(),
                    name: "ItemAnchor".to_owned(),
                    properties: vec![ManualPropertyDefinition {
                        name: "AnchorId".to_owned(),
                        edm_type: "Edm.String".to_owned(),
                        nullable: false,
                        max_length: None,
                        key_candidate: true,
                    }],
                },
            ],
            idempotency_key: "carry-create-manual-catalogue-0001".to_owned(),
        })
        .expect("create target manual catalogue");
    let after_catalogue = destination.workshop().expect("workshop after catalogue");
    let initial_draft = destination
        .start_draft(&StartDestinationDraftCommand {
            schema_version: START_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: after_catalogue.engagement_generation,
            catalogue_authority_id: manual_catalogue.catalogue_authority_id.clone(),
            expected_catalogue_hash: manual_catalogue.catalogue_hash.clone(),
            root_technical_entity: "CarryForward.HeaderAnchor".to_owned(),
            child_technical_entities: vec!["CarryForward.ItemAnchor".to_owned()],
            profile_id: SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID.to_owned(),
            title: "Synthetic basic sales document workspace profile".to_owned(),
            owner: "Destination practitioner".to_owned(),
            actor: "destination-practitioner".to_owned(),
            reason: "Begin a separately governed destination revision.".to_owned(),
            idempotency_key: "carry-start-destination-draft-0001".to_owned(),
        })
        .expect("start target destination draft");
    let after_start = destination.workshop().expect("workshop after draft start");
    let saved_draft = destination
        .save_draft(&SaveDestinationDraftCommand {
            schema_version: SAVE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: after_start.engagement_generation,
            draft_id: initial_draft.draft_id,
            expected_draft_generation: initial_draft.generation,
            expected_draft_hash: initial_draft.draft_hash,
            snapshot: carried_workspace_profile_snapshot(),
            actor: "destination-practitioner".to_owned(),
            reason: "Confirm exact retained semantics and one explicit new destination field."
                .to_owned(),
            idempotency_key: "carry-save-destination-draft-0001".to_owned(),
        })
        .expect("save compatible target destination draft");
    assert!(
        saved_draft.freeze_blockers.is_empty(),
        "{:?}",
        saved_draft.freeze_blockers
    );
    let after_save = destination.workshop().expect("workshop after draft save");
    let frozen = destination
        .freeze_draft(&FreezeDestinationDraftCommand {
            schema_version: FREEZE_DRAFT_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: after_save.engagement_generation,
            draft_id: saved_draft.draft_id,
            expected_draft_generation: saved_draft.generation,
            expected_draft_hash: saved_draft.draft_hash,
            profile_version: "1.1.0".to_owned(),
            acknowledgement: DESTINATION_FREEZE_ACKNOWLEDGEMENT.to_owned(),
            actor: "destination-practitioner".to_owned(),
            reason: "Freeze this exact reviewed destination revision.".to_owned(),
            idempotency_key: "carry-freeze-destination-0001".to_owned(),
        })
        .expect("freeze exact target destination profile");
    let approved = destination
        .approve_destination_profile(&ApproveDestinationProfileCommand {
            schema_version: APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: frozen.engagement_generation,
            profile_id: frozen.profile.profile_id.clone(),
            profile_version: frozen.profile.profile_version.clone(),
            profile_hash: frozen.profile.profile_hash.clone(),
            origin_binding_hash: frozen.origin_binding_hash.clone(),
            actor: "destination-approver".to_owned(),
            reason: "Approve this exact workspace profile for local mapping and validation only."
                .to_owned(),
            authority_acknowledgement: LOCAL_DESTINATION_APPROVAL_ACKNOWLEDGEMENT.to_owned(),
            idempotency_key: "carry-approve-destination-0001".to_owned(),
        })
        .expect("separately approve target destination profile");
    assert_eq!(approved.profile.profile_version, "1.1.0");

    let candidate_workshop = destination
        .workshop()
        .expect("workshop with reviewed target candidate");
    let candidate = candidate_workshop
        .reviewed_candidate
        .as_ref()
        .expect("reviewed target candidate");
    assert_eq!(
        candidate.profile.profile_hash,
        approved.profile.profile_hash
    );
    assert_eq!(
        candidate_workshop
            .active_destination
            .as_ref()
            .expect("source destination remains active")
            .profile_version,
        "1.0.0"
    );
    let exact_source = candidate_workshop
        .eligible_carry_forward_source
        .clone()
        .expect("source remains independently eligible after target approval");
    assert_eq!(exact_source, eligible);

    let persistence_snapshot = || {
        let connection = rusqlite::Connection::open(temporary.path().join("state.sqlite3"))
            .expect("inspect carry-forward persistence");
        let carry_forwards: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM destination_mapping_carry_forwards",
                [],
                |row| row.get(0),
            )
            .expect("count carry-forward results");
        let mapping_drafts: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM engagement_mapping_draft_heads",
                [],
                |row| row.get(0),
            )
            .expect("count mapping draft heads");
        let carry_attempts: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM destination_workshop_attempts
                 WHERE command_kind = 'CARRY_FORWARD_MAPPING'",
                [],
                |row| row.get(0),
            )
            .expect("count carry-forward attempts");
        let active: (
            i64,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = connection
            .query_row(
                "SELECT generation, selected_profile_id, selected_profile_version,
                        selected_profile_hash, active_mapping_id,
                        active_mapping_version, active_mapping_hash
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
                    ))
                },
            )
            .expect("read active carry-forward context");
        (carry_forwards, mapping_drafts, carry_attempts, active)
    };
    let before_rejections = persistence_snapshot();
    let wrong_source = destination
        .carry_forward_mapping(&CarryForwardMappingCommand {
            schema_version: CARRY_FORWARD_MAPPING_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: candidate_workshop.engagement_generation,
            source_mapping_id: exact_source.mapping_id.clone(),
            source_mapping_version: exact_source.mapping_version.clone(),
            source_mapping_hash: "0".repeat(64),
            target_profile_id: candidate.profile.profile_id.clone(),
            target_profile_version: candidate.profile.profile_version.clone(),
            target_profile_hash: candidate.profile.profile_hash.clone(),
            actor: "mapping-practitioner".to_owned(),
            reason: "This arbitrary source hash must not carry authority.".to_owned(),
            idempotency_key: "carry-reject-arbitrary-source-0001".to_owned(),
        })
        .expect_err("arbitrary unapproved source mapping must fail closed");
    assert!(matches!(wrong_source, KernelError::Conflict(_)));
    let wrong_target = destination
        .carry_forward_mapping(&CarryForwardMappingCommand {
            schema_version: CARRY_FORWARD_MAPPING_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: candidate_workshop.engagement_generation,
            source_mapping_id: exact_source.mapping_id.clone(),
            source_mapping_version: exact_source.mapping_version.clone(),
            source_mapping_hash: exact_source.mapping_hash.clone(),
            target_profile_id: exact_source.profile_id.clone(),
            target_profile_version: exact_source.profile_version.clone(),
            target_profile_hash: exact_source.profile_hash.clone(),
            actor: "mapping-practitioner".to_owned(),
            reason: "The active source profile is not the reviewed target candidate.".to_owned(),
            idempotency_key: "carry-reject-non-current-target-0001".to_owned(),
        })
        .expect_err("same and non-current target profile must fail closed");
    assert!(matches!(wrong_target, KernelError::Conflict(_)));
    let stale = destination
        .carry_forward_mapping(&CarryForwardMappingCommand {
            schema_version: CARRY_FORWARD_MAPPING_COMMAND_SCHEMA.to_owned(),
            expected_engagement_generation: candidate_workshop
                .engagement_generation
                .saturating_sub(1),
            source_mapping_id: exact_source.mapping_id.clone(),
            source_mapping_version: exact_source.mapping_version.clone(),
            source_mapping_hash: exact_source.mapping_hash.clone(),
            target_profile_id: candidate.profile.profile_id.clone(),
            target_profile_version: candidate.profile.profile_version.clone(),
            target_profile_hash: candidate.profile.profile_hash.clone(),
            actor: "mapping-practitioner".to_owned(),
            reason: "A stale command must not advance destination authority.".to_owned(),
            idempotency_key: "carry-reject-stale-generation-0001".to_owned(),
        })
        .expect_err("stale carry-forward command must fail closed");
    assert!(matches!(stale, KernelError::Conflict(_)));
    assert_eq!(
        persistence_snapshot(),
        before_rejections,
        "rejected carry-forward commands changed durable state"
    );

    let exact_command = CarryForwardMappingCommand {
        schema_version: CARRY_FORWARD_MAPPING_COMMAND_SCHEMA.to_owned(),
        expected_engagement_generation: candidate_workshop.engagement_generation,
        source_mapping_id: exact_source.mapping_id.clone(),
        source_mapping_version: exact_source.mapping_version.clone(),
        source_mapping_hash: exact_source.mapping_hash.clone(),
        target_profile_id: candidate.profile.profile_id.clone(),
        target_profile_version: candidate.profile.profile_version.clone(),
        target_profile_hash: candidate.profile.profile_hash.clone(),
        actor: "mapping-practitioner".to_owned(),
        reason:
            "Carry forward only exact compatible bindings into explicit review for the new profile."
                .to_owned(),
        idempotency_key: "carry-exact-command-0001".to_owned(),
    };
    let carried = destination
        .carry_forward_mapping(&exact_command)
        .expect("carry forward exact approved mapping authority");
    assert_eq!(carried.cloned_binding_count, 13);
    assert_eq!(carried.unresolved_target_count, 2);
    assert_eq!(carried.unresolved_relationship_count, 0);
    assert_eq!(
        carried.unresolved_relationships,
        [] as [migration_stream::destination::UnresolvedCarryForwardRelationshipView; 0]
    );
    let retry = destination
        .carry_forward_mapping(&exact_command)
        .expect("retry exact immutable carry-forward");
    assert_eq!(retry, carried);

    let active = engagement
        .state()
        .expect("state after mapping carry-forward");
    assert_eq!(
        active.active.profile_id.as_deref(),
        Some(SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID)
    );
    assert_eq!(active.active.profile_version.as_deref(), Some("1.1.0"));
    assert_eq!(
        active.active.profile_hash.as_deref(),
        Some(candidate.profile.profile_hash.as_str())
    );
    assert!(active.active.mapping_id.is_none());
    assert!(active.active.mapping_version.is_none());
    assert!(active.active.mapping_hash.is_none());
    assert_eq!(active.mapping_stage, "NOT_PREPARED");
    assert_eq!(active.validation_stage, "NOT_LAUNCHED");

    let mapping_workspace = engagement
        .mapping_workspace()
        .expect("new target mapping workspace");
    let carry_draft = mapping_workspace.draft.expect("carried mapping draft");
    assert_eq!(carry_draft.state, "EDITABLE");
    assert_eq!(carry_draft.snapshot.source_entity_assignments.len(), 2);
    assert_eq!(carry_draft.snapshot.bindings.len(), 13);
    assert_eq!(carry_draft.snapshot.relationship_bindings.len(), 1);
    assert_eq!(
        carry_draft.snapshot.reference_tables,
        [] as [migration_stream::mapping::ReferenceTable; 0]
    );
    assert!(
        carry_draft
            .snapshot
            .bindings
            .iter()
            .all(|binding| binding.target_field_id != "documentType")
    );
    let connection = rusqlite::Connection::open(temporary.path().join("state.sqlite3"))
        .expect("read canonical carry-forward result");
    let canonical_result: String = connection
        .query_row(
            "SELECT canonical_result_json
             FROM destination_mapping_carry_forwards
             WHERE carry_forward_id = ?1",
            [&carried.carry_forward_id],
            |row| row.get(0),
        )
        .expect("canonical carry-forward result");
    let result: serde_json::Value =
        serde_json::from_str(&canonical_result).expect("parse carry-forward result");
    assert_eq!(
        result["unresolvedTargets"],
        serde_json::json!([
            ["SalesDocument", "deliveryNote"],
            ["SalesDocument", "documentType"]
        ])
    );
    assert_eq!(
        result["sourceMapping"]["mappingApprovalId"],
        exact_source.mapping_approval_id
    );
    assert_eq!(
        result["sourceMapping"]["mappingApprovalHash"],
        exact_source.mapping_approval_hash
    );
    assert_eq!(
        result["targetProfile"]["originBindingHash"],
        candidate.origin_binding_hash
    );
    drop(connection);

    let mut resolved_snapshot = carry_draft.snapshot.clone();
    resolved_snapshot.bindings.push(binding(
        "carry.resolved.document-type",
        HEADER_ENTITY,
        "deal_book.csv",
        "route_code",
        3,
        "documentType",
        MappingKind::Direct,
        None,
    ));
    resolved_snapshot.bindings.push(binding(
        "carry.resolved.delivery-note",
        HEADER_ENTITY,
        "deal_book.csv",
        "buy_ref",
        5,
        "deliveryNote",
        MappingKind::EmptyAsNull,
        None,
    ));
    resolved_snapshot.author = Some("mapping-practitioner".to_owned());
    let resolved = engagement
        .save_mapping_draft(&SaveMappingDraftCommand {
            schema_version: SAVE_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "carry-save-resolved-target-0001".to_owned(),
            expected_engagement_generation: mapping_workspace.engagement_generation,
            expected_draft_id: Some(carry_draft.draft_id.clone()),
            expected_draft_generation: Some(carry_draft.draft_generation),
            expected_draft_hash: Some(carry_draft.draft_hash.clone()),
            snapshot: resolved_snapshot,
        })
        .expect("save exact fully resolved carried mapping generation");
    assert_eq!(resolved.draft.draft_generation, 2);
    assert_eq!(resolved.draft.snapshot.bindings.len(), 15);

    let mapping_files = || {
        fs::read_dir(temporary.path().join("engagement/mappings"))
            .expect("read immutable mapping evidence directory")
            .map(|entry| {
                entry
                    .expect("read immutable mapping evidence entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<BTreeSet<_>>()
    };
    let before_failed_submission = engagement
        .state()
        .expect("state before rejected carried mapping submissions");
    let files_before_failed_submission = mapping_files();
    let non_semantic_version = engagement
        .submit_mapping_draft(&SubmitMappingDraftCommand {
            schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "carry-reject-non-semver-0001".to_owned(),
            expected_engagement_generation: resolved.engagement_generation,
            draft_id: resolved.draft.draft_id.clone(),
            expected_draft_generation: resolved.draft.draft_generation,
            expected_draft_hash: resolved.draft.draft_hash.clone(),
            mapping_id: exact_source.mapping_id.clone(),
            version: "next".to_owned(),
            acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect_err("operator mapping version outside major.minor.patch must fail closed");
    assert!(matches!(
        non_semantic_version,
        KernelError::MappingInvalid(_)
    ));
    assert_eq!(
        engagement
            .state()
            .expect("state after rejected non-semantic mapping version"),
        before_failed_submission
    );
    assert_eq!(mapping_files(), files_before_failed_submission);

    let immutable_pair_collision = engagement
        .submit_mapping_draft(&SubmitMappingDraftCommand {
            schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "carry-reject-owned-pair-0001".to_owned(),
            expected_engagement_generation: resolved.engagement_generation,
            draft_id: resolved.draft.draft_id.clone(),
            expected_draft_generation: resolved.draft.draft_generation,
            expected_draft_hash: resolved.draft.draft_hash.clone(),
            mapping_id: exact_source.mapping_id.clone(),
            version: exact_source.mapping_version.clone(),
            acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect_err("prior immutable mapping ID/version pair must not be rewritten");
    assert!(matches!(
        immutable_pair_collision,
        KernelError::MappingConflict(_)
    ));
    assert_eq!(
        engagement
            .state()
            .expect("state after rejected immutable mapping pair"),
        before_failed_submission,
        "mapping collision advanced active authority"
    );
    assert_eq!(
        mapping_files(),
        files_before_failed_submission,
        "mapping collision left orphan immutable bytes"
    );

    let submitted_target = engagement
        .submit_mapping_draft(&SubmitMappingDraftCommand {
            schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "carry-submit-target-mapping-0001".to_owned(),
            expected_engagement_generation: resolved.engagement_generation,
            draft_id: resolved.draft.draft_id.clone(),
            expected_draft_generation: resolved.draft.draft_generation,
            expected_draft_hash: resolved.draft.draft_hash.clone(),
            mapping_id: exact_source.mapping_id.clone(),
            version: "1.1.0".to_owned(),
            acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("submit carried mapping under a new immutable version");
    let submitted_target_workspace = engagement
        .mapping_workspace()
        .expect("reopen submitted target mapping");
    let submitted_target_draft = submitted_target_workspace
        .draft
        .expect("submitted target mapping draft");
    let approved_target = engagement
        .approve_mapping_version(&ApproveMappingVersionCommand {
            schema_version: APPROVE_MAPPING_VERSION_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "carry-approve-target-mapping-0001".to_owned(),
            expected_engagement_generation: submitted_target_workspace.engagement_generation,
            draft_id: submitted_target_draft.draft_id.clone(),
            expected_draft_generation: submitted_target_draft.draft_generation,
            expected_draft_hash: submitted_target_draft.draft_hash.clone(),
            expected_mapping_id: exact_source.mapping_id.clone(),
            expected_mapping_version: "1.1.0".to_owned(),
            expected_mapping_hash: submitted_target.mapping_hash.clone(),
            actor: "mapping-reviewer".to_owned(),
            reason: "Approve the exact reviewed carry-forward mapping for the new profile."
                .to_owned(),
            acknowledgement: OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("approve exact carried mapping hash");
    assert_eq!(approved_target.mapping_version, "1.1.0");
    let active_after_approval = engagement
        .state()
        .expect("active state after carried mapping approval");
    assert_eq!(active_after_approval.profile_stage, "APPROVED");
    assert_eq!(active_after_approval.mapping_stage, "VALIDATED");
    assert_eq!(
        active_after_approval.active.profile_hash.as_deref(),
        Some(candidate.profile.profile_hash.as_str())
    );
    assert_eq!(
        active_after_approval.active.mapping_id.as_deref(),
        Some(exact_source.mapping_id.as_str())
    );
    assert_eq!(
        active_after_approval.active.mapping_version.as_deref(),
        Some("1.1.0")
    );
    assert_eq!(
        active_after_approval.active.mapping_hash.as_deref(),
        Some(submitted_target.mapping_hash.as_str())
    );

    let old_mapping = migration_stream::mapping::inspect_mapping(
        temporary.path(),
        &exact_source.mapping_id,
        &exact_source.mapping_version,
    )
    .expect("old immutable mapping remains inspectable");
    assert_eq!(old_mapping.mapping_hash, exact_source.mapping_hash);
    assert_eq!(old_mapping.profile.version, exact_source.profile_version);
    let new_mapping = migration_stream::mapping::inspect_mapping(
        temporary.path(),
        &approved_target.mapping_id,
        &approved_target.mapping_version,
    )
    .expect("new carried mapping is independently inspectable");
    assert_eq!(new_mapping.mapping_hash, approved_target.mapping_hash);
    assert_eq!(new_mapping.profile.version, "1.1.0");
    assert_ne!(new_mapping.mapping_hash, old_mapping.mapping_hash);

    let restarted_engagement = sales_service(temporary.path());
    let restarted_destination = DestinationService::new(
        temporary.path(),
        repository_path("schemas/migration-object-profile.schema.json"),
    );
    let reopened_workshop = restarted_destination
        .workshop()
        .expect("reopen destination workshop after carry-forward");
    let reopened_active = reopened_workshop
        .active_destination
        .expect("reopened active destination");
    assert_eq!(reopened_active.profile_version, "1.1.0");
    assert_eq!(reopened_active.profile_hash, candidate.profile.profile_hash);
    let reopened_eligible = reopened_workshop
        .eligible_carry_forward_source
        .expect("approved carried mapping is the exact active source authority");
    assert_eq!(reopened_eligible.mapping_id, approved_target.mapping_id);
    assert_eq!(reopened_eligible.mapping_version, "1.1.0");
    assert_eq!(reopened_eligible.mapping_hash, approved_target.mapping_hash);
    let reopened_mapping = restarted_engagement
        .mapping_workspace()
        .expect("reopen carried mapping draft");
    let reopened_draft = reopened_mapping
        .draft
        .expect("reopened carried mapping draft");
    assert_eq!(reopened_draft.state, "APPROVED");
    assert_eq!(reopened_draft.draft_id, submitted_target_draft.draft_id);
    assert_eq!(
        reopened_draft.submitted_mapping_hash.as_deref(),
        Some(approved_target.mapping_hash.as_str())
    );
    let restarted_state = restarted_engagement
        .state()
        .expect("restart exact active carried mapping");
    assert_eq!(restarted_state, active_after_approval);
    let integrity = integrity_check(temporary.path()).expect("replay carry-forward authority");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

struct EditableSalesDraftFixture {
    temporary: TempDir,
    saved: SaveMappingDraftResult,
}

enum ConcurrentResult {
    Save(migration_stream::Result<()>),
    Submit(migration_stream::Result<()>),
}

fn prepare_editable_sales_draft() -> EditableSalesDraftFixture {
    let temporary = TempDir::new().expect("temporary concurrent-submit workspace");
    let service = sales_service(temporary.path());
    service
        .create_or_reopen(&CreateOrReopenCommand {
            schema_version: CREATE_ENGAGEMENT_COMMAND_SCHEMA.to_owned(),
            display_name: "Concurrent sales mapping".to_owned(),
            idempotency_key: "sales-race-create-0001".to_owned(),
        })
        .expect("create concurrent-submit engagement");
    let reservation = service
        .reserve_preflight(&ReservePreflightCommand {
            schema_version: RESERVE_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            source_name: "unfamiliar-sales-race.zip".to_owned(),
            idempotency_key: "sales-race-reserve-0001".to_owned(),
        })
        .expect("reserve concurrent-submit source");
    let source_zip = temporary.path().join("sales-race-source.zip");
    build_sales_zip(&source_zip);
    fs::copy(&source_zip, &reservation.controlled_path)
        .expect("stream concurrent-submit source package");
    let (source_hash, source_size) = file_identity(&source_zip);
    let preflight = service
        .complete_preflight(&CompletePreflightCommand {
            preflight_id: reservation.preflight.preflight_id,
            sha256: source_hash.clone(),
            size_bytes: source_size,
        })
        .expect("complete concurrent-submit preflight");
    service
        .accept_preflight(&AcceptPreflightCommand {
            schema_version: ACCEPT_SOURCE_COMMAND_SCHEMA.to_owned(),
            preflight_id: preflight.preflight_id,
            expected_source_hash: source_hash,
            expected_manifest_hash: preflight.manifest_hash.expect("race manifest hash"),
            idempotency_key: "sales-race-accept-0001".to_owned(),
            acknowledgement: SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("accept concurrent-submit source");
    service
        .select_object_pack(&SelectObjectPackCommand {
            schema_version: SELECT_OBJECT_PACK_COMMAND_SCHEMA.to_owned(),
            object_pack_id: SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID.to_owned(),
            idempotency_key: "sales-race-select-pack-0001".to_owned(),
        })
        .expect("select concurrent-submit pack");
    service
        .approve_profile(&ApproveProfileCommand {
            schema_version: APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA.to_owned(),
            actor: "race-profile-reviewer".to_owned(),
            reason:
                "Approve the exact target-neutral profile for the submit serialization regression."
                    .to_owned(),
            authority_acknowledgement: LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT
                .to_owned(),
            idempotency_key: "sales-race-approve-profile-0001".to_owned(),
        })
        .expect("approve concurrent-submit profile");
    let workspace = service
        .mapping_workspace()
        .expect("concurrent-submit mapping workspace");
    let saved = service
        .save_mapping_draft(&SaveMappingDraftCommand {
            schema_version: SAVE_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "sales-race-save-initial-0001".to_owned(),
            expected_engagement_generation: workspace.engagement_generation,
            expected_draft_id: None,
            expected_draft_generation: None,
            expected_draft_hash: None,
            snapshot: governed_snapshot(),
        })
        .expect("save initial concurrent-submit draft");
    EditableSalesDraftFixture { temporary, saved }
}

fn operator_mapping_files(workspace: &Path) -> Vec<std::ffi::OsString> {
    let mapping_directory = workspace.join("engagement/mappings");
    if !mapping_directory.exists() {
        return Vec::new();
    }
    fs::read_dir(&mapping_directory)
        .expect("read mapping evidence directory")
        .map(|entry| entry.expect("mapping directory entry").file_name())
        .filter(|name| name.to_string_lossy().starts_with("operator-"))
        .collect()
}

#[test]
#[allow(clippy::too_many_lines)]
fn concurrent_save_lock_prevents_orphan_mapping_submission_evidence() {
    let fixture = prepare_editable_sales_draft();
    let workspace = fixture.temporary.path().to_path_buf();
    let mut revised_snapshot = governed_snapshot();
    revised_snapshot.author = Some("concurrent-draft-editor".to_owned());
    let save_command = SaveMappingDraftCommand {
        schema_version: SAVE_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
        idempotency_key: "sales-race-save-competing-0001".to_owned(),
        expected_engagement_generation: fixture.saved.engagement_generation,
        expected_draft_id: Some(fixture.saved.draft.draft_id.clone()),
        expected_draft_generation: Some(fixture.saved.draft.draft_generation),
        expected_draft_hash: Some(fixture.saved.draft.draft_hash.clone()),
        snapshot: revised_snapshot,
    };
    let submit_command = SubmitMappingDraftCommand {
        schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
        idempotency_key: "sales-race-submit-0001".to_owned(),
        expected_engagement_generation: fixture.saved.engagement_generation,
        draft_id: fixture.saved.draft.draft_id.clone(),
        expected_draft_generation: fixture.saved.draft.draft_generation,
        expected_draft_hash: fixture.saved.draft.draft_hash.clone(),
        mapping_id: "synthetic.sales-document.concurrent".to_owned(),
        version: "1.0.0".to_owned(),
        acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
    };
    let save_service = sales_service(&workspace);
    let submit_service = sales_service(&workspace);
    let database_path = workspace.join("state.sqlite3");
    let lock_path = workspace.join(".migration-stream.lock");
    let (locked_sender, locked_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    let (save_result_sender, save_result_receiver) = mpsc::channel();
    let save_handle = thread::spawn(move || {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .expect("open competing save lock");
        lock.lock_exclusive()
            .expect("hold competing save writer exclusion");
        locked_sender
            .send(())
            .expect("signal competing save lock acquisition");
        let save_result = match release_receiver.recv_timeout(RACE_TEST_TIMEOUT) {
            Ok(()) => {
                fs2::FileExt::unlock(&lock).expect("release competing save writer exclusion");
                save_service
                    .save_mapping_draft(&save_command)
                    .map_err(|error| format!("{error:?}"))
            }
            Err(error) => {
                fs2::FileExt::unlock(&lock)
                    .expect("release competing writer after harness timeout");
                Err(format!(
                    "submit-race harness did not release the writer within {RACE_TEST_TIMEOUT:?}: {error}"
                ))
            }
        };
        let _ = save_result_sender.send(save_result);
    });
    locked_receiver
        .recv_timeout(RACE_TEST_TIMEOUT)
        .expect("competing save did not acquire writer exclusion within the 10-second test bound");

    let submit_error = submit_service
        .submit_mapping_draft(&submit_command)
        .expect_err("submit must fail before file creation while the save holds the writer lock");
    assert!(
        matches!(submit_error, KernelError::WorkspaceLocked(_)),
        "{submit_error:?}"
    );
    release_sender
        .send(())
        .expect("release the competing save thread");
    save_result_receiver
        .recv_timeout(RACE_TEST_TIMEOUT)
        .expect("competing save did not finish within the 10-second test bound")
        .expect("competing save succeeds after writer exclusion is released");
    save_handle
        .join()
        .expect("competing save thread panicked after reporting its result");

    let connection =
        rusqlite::Connection::open(&database_path).expect("inspect serialized submit race");
    let mapping_versions: i64 = connection
        .query_row("SELECT COUNT(*) FROM mapping_versions", [], |row| {
            row.get(0)
        })
        .expect("count mapping versions");
    let preparations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_mapping_preparations",
            [],
            |row| row.get(0),
        )
        .expect("count mapping preparations");
    let submit_attempts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_mapping_workspace_attempts
             WHERE command_kind = 'SUBMIT_MAPPING'",
            [],
            |row| row.get(0),
        )
        .expect("count mapping SUBMIT attempts");
    assert_eq!(mapping_versions, 0);
    assert_eq!(preparations, 0);
    assert_eq!(submit_attempts, 0);
    let (generation, state): (i64, String) = connection
        .query_row(
            "SELECT generation, state FROM engagement_mapping_draft_heads
             WHERE draft_id = ?1",
            [&fixture.saved.draft.draft_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read winning editable draft head");
    assert_eq!(
        generation,
        i64::try_from(fixture.saved.draft.draft_generation + 1).expect("draft generation")
    );
    assert_eq!(state, "EDITABLE");
    drop(connection);

    let operator_files = operator_mapping_files(&workspace);
    assert!(
        operator_files.is_empty(),
        "submit retained orphan operator mapping files: {operator_files:?}"
    );
    let integrity = integrity_check(&workspace).expect("replay serialized submit race");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn simultaneous_save_and_submit_leave_one_complete_authority_outcome() {
    let fixture = prepare_editable_sales_draft();
    let workspace = fixture.temporary.path().to_path_buf();
    let mut revised_snapshot = governed_snapshot();
    revised_snapshot.author = Some("simultaneous-draft-editor".to_owned());
    let save_command = SaveMappingDraftCommand {
        schema_version: SAVE_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
        idempotency_key: "sales-simultaneous-save-0001".to_owned(),
        expected_engagement_generation: fixture.saved.engagement_generation,
        expected_draft_id: Some(fixture.saved.draft.draft_id.clone()),
        expected_draft_generation: Some(fixture.saved.draft.draft_generation),
        expected_draft_hash: Some(fixture.saved.draft.draft_hash.clone()),
        snapshot: revised_snapshot,
    };
    let submit_command = SubmitMappingDraftCommand {
        schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
        idempotency_key: "sales-simultaneous-submit-0001".to_owned(),
        expected_engagement_generation: fixture.saved.engagement_generation,
        draft_id: fixture.saved.draft.draft_id.clone(),
        expected_draft_generation: fixture.saved.draft.draft_generation,
        expected_draft_hash: fixture.saved.draft.draft_hash.clone(),
        mapping_id: "synthetic.sales-document.simultaneous".to_owned(),
        version: "1.0.0".to_owned(),
        acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
    };
    let barrier = Arc::new(Barrier::new(3));
    let save_service = sales_service(&workspace);
    let submit_service = sales_service(&workspace);
    let (result_sender, result_receiver) = mpsc::channel();
    let save_barrier = Arc::clone(&barrier);
    let save_result_sender = result_sender.clone();
    let save_handle = thread::spawn(move || {
        save_barrier.wait();
        let result = save_service.save_mapping_draft(&save_command).map(|_| ());
        let _ = save_result_sender.send(ConcurrentResult::Save(result));
    });
    let submit_barrier = Arc::clone(&barrier);
    let submit_handle = thread::spawn(move || {
        submit_barrier.wait();
        let result = submit_service
            .submit_mapping_draft(&submit_command)
            .map(|_| ());
        let _ = result_sender.send(ConcurrentResult::Submit(result));
    });
    barrier.wait();
    let mut save_result = None;
    let mut submit_result = None;
    for _ in 0..2 {
        match result_receiver
            .recv_timeout(RACE_TEST_TIMEOUT)
            .expect("simultaneous command did not finish within the 10-second test bound")
        {
            ConcurrentResult::Save(result) => save_result = Some(result),
            ConcurrentResult::Submit(result) => submit_result = Some(result),
        }
    }
    let save_result = save_result.expect("simultaneous save thread did not report a result");
    let submit_result = submit_result.expect("simultaneous submit thread did not report a result");
    save_handle
        .join()
        .expect("simultaneous save thread panicked after reporting");
    submit_handle
        .join()
        .expect("simultaneous submit thread panicked after reporting");
    assert_ne!(
        save_result.is_ok(),
        submit_result.is_ok(),
        "exactly one competing command must commit: save={save_result:?}, submit={submit_result:?}"
    );
    for error in [
        save_result.as_ref().err().map(KernelError::code),
        submit_result.as_ref().err().map(KernelError::code),
    ]
    .into_iter()
    .flatten()
    {
        assert!(
            matches!(error, "CONFLICT" | "WORKSPACE_LOCKED"),
            "unexpected competing command error: {error}"
        );
    }

    let connection = rusqlite::Connection::open(workspace.join("state.sqlite3"))
        .expect("inspect simultaneous command outcome");
    let mapping_versions: i64 = connection
        .query_row("SELECT COUNT(*) FROM mapping_versions", [], |row| {
            row.get(0)
        })
        .expect("count simultaneous mapping versions");
    let preparations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_mapping_preparations",
            [],
            |row| row.get(0),
        )
        .expect("count simultaneous mapping preparations");
    let submit_attempts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_mapping_workspace_attempts
             WHERE command_kind = 'SUBMIT_MAPPING'",
            [],
            |row| row.get(0),
        )
        .expect("count simultaneous SUBMIT attempts");
    let (generation, state): (i64, String) = connection
        .query_row(
            "SELECT generation, state FROM engagement_mapping_draft_heads
             WHERE draft_id = ?1",
            [&fixture.saved.draft.draft_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read simultaneous draft head");
    drop(connection);

    let operator_files = operator_mapping_files(&workspace);
    if submit_result.is_ok() {
        assert_eq!(mapping_versions, 1);
        assert_eq!(preparations, 1);
        assert_eq!(submit_attempts, 1);
        assert_eq!(
            generation,
            i64::try_from(fixture.saved.draft.draft_generation).expect("submitted generation")
        );
        assert_eq!(state, "SUBMITTED");
        assert_eq!(operator_files.len(), 1);
    } else {
        assert_eq!(mapping_versions, 0);
        assert_eq!(preparations, 0);
        assert_eq!(submit_attempts, 0);
        assert_eq!(
            generation,
            i64::try_from(fixture.saved.draft.draft_generation + 1)
                .expect("advanced editable generation")
        );
        assert_eq!(state, "EDITABLE");
        assert_eq!(operator_files, [] as [std::ffi::OsString; 0]);
    }
    let integrity = integrity_check(&workspace).expect("replay simultaneous command outcome");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
fn failed_final_submit_transaction_removes_only_its_new_mapping_file() {
    let fixture = prepare_editable_sales_draft();
    let workspace = fixture.temporary.path().to_path_buf();
    let database_path = workspace.join("state.sqlite3");
    let connection =
        rusqlite::Connection::open(&database_path).expect("open rollback regression database");
    connection
        .execute_batch(
            "CREATE TRIGGER force_mapping_preparation_failure
                 BEFORE INSERT ON engagement_mapping_preparations
                 BEGIN
                     SELECT RAISE(ABORT, 'forced mapping preparation failure');
                 END;",
        )
        .expect("install bounded final-transaction failure");
    drop(connection);

    let service = sales_service(&workspace);
    service
        .submit_mapping_draft(&SubmitMappingDraftCommand {
            schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "sales-rollback-submit-0001".to_owned(),
            expected_engagement_generation: fixture.saved.engagement_generation,
            draft_id: fixture.saved.draft.draft_id.clone(),
            expected_draft_generation: fixture.saved.draft.draft_generation,
            expected_draft_hash: fixture.saved.draft.draft_hash.clone(),
            mapping_id: "synthetic.sales-document.rollback".to_owned(),
            version: "1.0.0".to_owned(),
            acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect_err("forced final-transaction failure must reject submission");

    let connection =
        rusqlite::Connection::open(&database_path).expect("inspect rolled-back submission");
    connection
        .execute_batch("DROP TRIGGER force_mapping_preparation_failure")
        .expect("remove bounded final-transaction failure");
    let mapping_versions: i64 = connection
        .query_row("SELECT COUNT(*) FROM mapping_versions", [], |row| {
            row.get(0)
        })
        .expect("count rolled-back mapping versions");
    let valid_attempts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM mapping_validation_attempts WHERE state = 'VALID'",
            [],
            |row| row.get(0),
        )
        .expect("count rolled-back valid mapping attempts");
    let preparations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_mapping_preparations",
            [],
            |row| row.get(0),
        )
        .expect("count rolled-back mapping preparations");
    let submit_attempts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_mapping_workspace_attempts
             WHERE command_kind = 'SUBMIT_MAPPING'",
            [],
            |row| row.get(0),
        )
        .expect("count rolled-back submit attempts");
    let (generation, state): (i64, String) = connection
        .query_row(
            "SELECT generation, state FROM engagement_mapping_draft_heads
             WHERE draft_id = ?1",
            [&fixture.saved.draft.draft_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read draft head after rolled-back submission");
    assert_eq!(mapping_versions, 0);
    assert_eq!(valid_attempts, 0);
    assert_eq!(preparations, 0);
    assert_eq!(submit_attempts, 0);
    assert_eq!(
        generation,
        i64::try_from(fixture.saved.draft.draft_generation).expect("unchanged draft generation")
    );
    assert_eq!(state, "EDITABLE");
    drop(connection);
    assert!(
        operator_mapping_files(&workspace).is_empty(),
        "failed transaction retained its newly created operator mapping file"
    );
    let integrity = integrity_check(&workspace).expect("replay rolled-back mapping submission");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn conflicting_preparation_identity_rolls_back_exact_mapping_submission() {
    let fixture = prepare_editable_sales_draft();
    let workspace = fixture.temporary.path().to_path_buf();
    let database_path = workspace.join("state.sqlite3");
    let connection =
        rusqlite::Connection::open(&database_path).expect("open preparation-conflict database");
    let (engagement_id, job_id, source_hash, profile_id, profile_version, profile_hash): (
        String,
        String,
        String,
        String,
        String,
        String,
    ) = connection
        .query_row(
            "SELECT c.engagement_id, c.active_job_id, j.source_hash,
                    c.selected_profile_id, c.selected_profile_version,
                    c.selected_profile_hash
             FROM engagement_active_context c
             JOIN jobs j ON j.id = c.active_job_id",
            [],
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
        .expect("read active preparation authority");
    let mut prior_snapshot = governed_snapshot();
    prior_snapshot.author = Some("prior-preparation-author".to_owned());
    let prior_document = MappingDocument {
        schema_version: migration_stream::mapping::MAPPING_SCHEMA_VERSION,
        mapping_id: "synthetic.sales-document.preparation-conflict".to_owned(),
        version: "1.0.0".to_owned(),
        profile: MappingProfileIdentity {
            profile_id: profile_id.clone(),
            version: profile_version.clone(),
            sha256: profile_hash.clone(),
        },
        source: MappingSourceIdentity {
            package_sha256: source_hash.clone(),
            source_files: vec!["deal_book.csv".to_owned(), "deal_detail.csv".to_owned()],
        },
        root_entity_id: HEADER_ENTITY.to_owned(),
        source_entity_assignments: prior_snapshot.source_entity_assignments,
        bindings: prior_snapshot.bindings,
        reference_tables: prior_snapshot.reference_tables,
        relationship_bindings: prior_snapshot.relationship_bindings,
        author: prior_snapshot.author,
        reviewer: None,
    };
    let prior_value = serde_json::to_value(prior_document).expect("prior preparation value");
    let prior_json = canonical_json_string(&prior_value).expect("prior preparation canonical JSON");
    let prior_hash = canonical_hash(&prior_value).expect("prior preparation hash");
    connection
        .execute(
            "INSERT INTO engagement_mapping_preparations (
                 preparation_id, engagement_id, mapping_id, mapping_version,
                 mapping_hash, document_json, job_id, source_hash,
                 profile_id, profile_version, profile_hash, created_at
             ) VALUES (
                 'prior-preparation-conflict', ?1,
                 'synthetic.sales-document.preparation-conflict', '1.0.0',
                 ?2, ?3, ?4, ?5, ?6, ?7, ?8, '2026-07-28T00:00:00Z'
             )",
            rusqlite::params![
                engagement_id,
                prior_hash,
                prior_json,
                job_id,
                source_hash,
                profile_id,
                profile_version,
                profile_hash,
            ],
        )
        .expect("insert legitimate conflicting preparation");
    drop(connection);

    let service = sales_service(&workspace);
    let error = service
        .submit_mapping_draft(&SubmitMappingDraftCommand {
            schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "sales-preparation-conflict-submit-0001".to_owned(),
            expected_engagement_generation: fixture.saved.engagement_generation,
            draft_id: fixture.saved.draft.draft_id.clone(),
            expected_draft_generation: fixture.saved.draft.draft_generation,
            expected_draft_hash: fixture.saved.draft.draft_hash.clone(),
            mapping_id: "synthetic.sales-document.preparation-conflict".to_owned(),
            version: "1.0.0".to_owned(),
            acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect_err("different immutable preparation bytes must fail closed");
    assert!(
        matches!(error, KernelError::Conflict(ref detail) if detail.contains("preparation differs")),
        "{error:?}"
    );

    let connection =
        rusqlite::Connection::open(&database_path).expect("inspect preparation-conflict rollback");
    let retained: (String, String, String) = connection
        .query_row(
            "SELECT preparation_id, mapping_hash, document_json
             FROM engagement_mapping_preparations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read retained prior preparation");
    assert_eq!(retained.0, "prior-preparation-conflict");
    assert_eq!(retained.1, prior_hash);
    assert_eq!(retained.2, prior_json);
    let valid_attempts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM mapping_validation_attempts WHERE state = 'VALID'",
            [],
            |row| row.get(0),
        )
        .expect("count preparation-conflict valid attempts");
    let mapping_versions: i64 = connection
        .query_row("SELECT COUNT(*) FROM mapping_versions", [], |row| {
            row.get(0)
        })
        .expect("count preparation-conflict mapping versions");
    let submit_attempts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_mapping_workspace_attempts
             WHERE command_kind = 'SUBMIT_MAPPING'",
            [],
            |row| row.get(0),
        )
        .expect("count preparation-conflict SUBMIT attempts");
    let (generation, state): (i64, String) = connection
        .query_row(
            "SELECT generation, state FROM engagement_mapping_draft_heads
             WHERE draft_id = ?1",
            [&fixture.saved.draft.draft_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read preparation-conflict draft head");
    assert_eq!(valid_attempts, 0);
    assert_eq!(mapping_versions, 0);
    assert_eq!(submit_attempts, 0);
    assert_eq!(
        generation,
        i64::try_from(fixture.saved.draft.draft_generation).expect("unchanged draft generation")
    );
    assert_eq!(state, "EDITABLE");
    drop(connection);
    assert_eq!(
        operator_mapping_files(&workspace),
        [] as [std::ffi::OsString; 0]
    );
    let integrity = integrity_check(&workspace).expect("replay retained prior preparation");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[cfg(unix)]
#[test]
fn linked_mapping_ancestor_is_rejected_before_outside_or_authority_side_effects() {
    use std::os::unix::fs::symlink;

    let fixture = prepare_editable_sales_draft();
    let workspace = fixture.temporary.path().to_path_buf();
    let outside = TempDir::new().expect("outside mapping directory");
    let engagement_path = workspace.join("engagement");
    let retained_engagement_path = workspace.join("engagement-retained-for-link-test");
    fs::rename(&engagement_path, &retained_engagement_path)
        .expect("retain real engagement directory");
    symlink(outside.path(), &engagement_path).expect("install linked engagement ancestor");

    let service = sales_service(&workspace);
    let error = service
        .submit_mapping_draft(&SubmitMappingDraftCommand {
            schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "sales-linked-ancestor-submit-0001".to_owned(),
            expected_engagement_generation: fixture.saved.engagement_generation,
            draft_id: fixture.saved.draft.draft_id.clone(),
            expected_draft_generation: fixture.saved.draft.draft_generation,
            expected_draft_hash: fixture.saved.draft.draft_hash.clone(),
            mapping_id: "synthetic.sales-document.linked-ancestor".to_owned(),
            version: "1.0.0".to_owned(),
            acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect_err("linked mapping ancestor must fail before file creation");
    assert!(
        matches!(error, KernelError::IntegrityFailed(_)),
        "{error:?}"
    );
    assert!(
        !outside.path().join("mappings").exists(),
        "linked ancestor created an outside mapping directory"
    );

    let connection = rusqlite::Connection::open(workspace.join("state.sqlite3"))
        .expect("inspect linked-ancestor rejection");
    let mapping_attempts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM mapping_validation_attempts",
            [],
            |row| row.get(0),
        )
        .expect("count linked-ancestor mapping attempts");
    let mapping_versions: i64 = connection
        .query_row("SELECT COUNT(*) FROM mapping_versions", [], |row| {
            row.get(0)
        })
        .expect("count linked-ancestor mapping versions");
    let preparations: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_mapping_preparations",
            [],
            |row| row.get(0),
        )
        .expect("count linked-ancestor preparations");
    let submit_attempts: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_mapping_workspace_attempts
             WHERE command_kind = 'SUBMIT_MAPPING'",
            [],
            |row| row.get(0),
        )
        .expect("count linked-ancestor SUBMIT attempts");
    let (generation, state): (i64, String) = connection
        .query_row(
            "SELECT generation, state FROM engagement_mapping_draft_heads
             WHERE draft_id = ?1",
            [&fixture.saved.draft.draft_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read linked-ancestor draft head");
    assert_eq!(mapping_attempts, 0);
    assert_eq!(mapping_versions, 0);
    assert_eq!(preparations, 0);
    assert_eq!(submit_attempts, 0);
    assert_eq!(
        generation,
        i64::try_from(fixture.saved.draft.draft_generation).expect("unchanged draft generation")
    );
    assert_eq!(state, "EDITABLE");
    drop(connection);

    fs::remove_file(&engagement_path).expect("remove linked engagement ancestor");
    fs::rename(&retained_engagement_path, &engagement_path)
        .expect("restore real engagement directory");
    let integrity = integrity_check(&workspace).expect("replay linked-ancestor rejection");
    assert!(integrity.ok, "integrity issues: {:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn unfamiliar_sales_documents_reach_entity_and_relationship_scoped_first_issues() {
    let temporary = TempDir::new().expect("temporary workspace");
    let service = sales_service(temporary.path());
    service
        .create_or_reopen(&CreateOrReopenCommand {
            schema_version: CREATE_ENGAGEMENT_COMMAND_SCHEMA.to_owned(),
            display_name: "Unfamiliar sales documents".to_owned(),
            idempotency_key: "sales-create-0001".to_owned(),
        })
        .expect("create engagement");

    let reservation = service
        .reserve_preflight(&ReservePreflightCommand {
            schema_version: RESERVE_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            source_name: "unfamiliar-sales.zip".to_owned(),
            idempotency_key: "sales-reserve-0001".to_owned(),
        })
        .expect("reserve source");
    let source_zip = temporary.path().join("sales-source.zip");
    build_sales_zip(&source_zip);
    fs::copy(&source_zip, &reservation.controlled_path).expect("stream source package");
    let (source_hash, source_size) = file_identity(&source_zip);
    let preflight = service
        .complete_preflight(&CompletePreflightCommand {
            preflight_id: reservation.preflight.preflight_id.clone(),
            sha256: source_hash.clone(),
            size_bytes: source_size,
        })
        .expect("complete source preflight");
    assert_eq!(preflight.state, SourcePreflightState::Ready);
    let accepted = service
        .accept_preflight(&AcceptPreflightCommand {
            schema_version: ACCEPT_SOURCE_COMMAND_SCHEMA.to_owned(),
            preflight_id: preflight.preflight_id,
            expected_source_hash: source_hash,
            expected_manifest_hash: preflight.manifest_hash.expect("manifest hash"),
            idempotency_key: "sales-accept-0001".to_owned(),
            acknowledgement: SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("accept and sample-free profile");

    let connection =
        rusqlite::Connection::open(temporary.path().join("state.sqlite3")).expect("database");
    let retained_samples: Vec<String> = connection
        .prepare("SELECT samples_json FROM profiling_columns WHERE run_id = ?1")
        .expect("sample query")
        .query_map([&accepted.profiling_run_id], |row| row.get(0))
        .expect("sample rows")
        .collect::<Result<_, _>>()
        .expect("sample values");
    assert_eq!(retained_samples.len(), 14);
    assert!(retained_samples.iter().all(|samples| samples == "[]"));
    drop(connection);

    service
        .select_object_pack(&SelectObjectPackCommand {
            schema_version: SELECT_OBJECT_PACK_COMMAND_SCHEMA.to_owned(),
            object_pack_id: SYNTHETIC_SALES_DOCUMENT_OBJECT_PACK_ID.to_owned(),
            idempotency_key: "sales-select-pack-0001".to_owned(),
        })
        .expect("select exact reviewed sales-document pack");
    service
        .approve_profile(&ApproveProfileCommand {
            schema_version: APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA.to_owned(),
            actor: "local-practitioner".to_owned(),
            reason: "Approve the exact reviewed target-neutral profile for local mapping and validation."
                .to_owned(),
            authority_acknowledgement: LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT
                .to_owned(),
            idempotency_key: "sales-approve-profile-0001".to_owned(),
        })
        .expect("approve exact profile");

    let source_profile = service
        .source_profile_rules()
        .expect("source/profile/rules");
    assert!(!source_profile.stored_sample_values);
    assert_eq!(source_profile.source_columns.len(), 14);
    assert_eq!(source_profile.profile_fields.len(), 14);
    assert_eq!(source_profile.rules.len(), 19);

    let workspace = service.mapping_workspace().expect("mapping workspace");
    assert_eq!(workspace.source_files.len(), 2);
    assert_eq!(workspace.entities.len(), 2);
    assert_eq!(workspace.relationships.len(), 1);
    assert!(workspace.draft.is_none());
    let accepted_headers = workspace
        .source_files
        .iter()
        .map(|file| {
            (
                file.source_file.clone(),
                file.columns
                    .iter()
                    .map(|column| column.source_column.clone())
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        accepted_headers["deal_book.csv"],
        [
            "case_ref",
            "party_ref",
            "booked_on",
            "route_code",
            "money_iso",
            "buy_ref",
            "owning_desk",
        ]
    );
    assert_eq!(
        accepted_headers["deal_detail.csv"],
        [
            "case_link",
            "seq_ref",
            "offer_ref",
            "demand_qty",
            "measure",
            "net_rate",
            "need_by",
        ]
    );

    let mut invalid_identifier_snapshot = governed_snapshot();
    invalid_identifier_snapshot.bindings[0].binding_id =
        "operator.SalesDocument.sourceDocumentId".to_owned();
    let invalid_identifier = service
        .save_mapping_draft(&SaveMappingDraftCommand {
            schema_version: SAVE_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "sales-invalid-id-0001".to_owned(),
            expected_engagement_generation: workspace.engagement_generation,
            expected_draft_id: None,
            expected_draft_generation: None,
            expected_draft_hash: None,
            snapshot: invalid_identifier_snapshot,
        })
        .expect_err("invalid immutable mapping identifiers must not be persisted");
    assert!(matches!(invalid_identifier, KernelError::MappingInvalid(_)));
    let unchanged_workspace = service
        .mapping_workspace()
        .expect("workspace remains unchanged after invalid draft");
    assert_eq!(
        unchanged_workspace.engagement_generation,
        workspace.engagement_generation
    );
    assert!(unchanged_workspace.draft.is_none());

    let save_command = SaveMappingDraftCommand {
        schema_version: SAVE_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
        idempotency_key: "sales-save-draft-0001".to_owned(),
        expected_engagement_generation: workspace.engagement_generation,
        expected_draft_id: None,
        expected_draft_generation: None,
        expected_draft_hash: None,
        snapshot: governed_snapshot(),
    };
    let saved = service
        .save_mapping_draft(&save_command)
        .expect("save durable mapping draft");
    assert_eq!(saved.draft.draft_generation, 1);
    assert_eq!(saved.draft.state, "EDITABLE");
    assert!(!saved.idempotent_replay);
    let replayed_save = service
        .save_mapping_draft(&save_command)
        .expect("replay identical mapping draft save");
    assert!(replayed_save.idempotent_replay);
    assert_eq!(replayed_save.draft, saved.draft);

    let stale = service
        .save_mapping_draft(&SaveMappingDraftCommand {
            idempotency_key: "sales-save-draft-stale".to_owned(),
            ..save_command.clone()
        })
        .expect_err("stale generation must fail closed");
    assert!(matches!(stale, KernelError::Conflict(_)));

    let restarted = sales_service(temporary.path());
    let reloaded = restarted
        .mapping_workspace()
        .expect("reload mapping workspace after restart");
    assert_eq!(reloaded.draft.as_ref(), Some(&saved.draft));
    assert_eq!(reloaded.engagement_generation, saved.engagement_generation);

    let submitted = restarted
        .submit_mapping_draft(&SubmitMappingDraftCommand {
            schema_version: SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "sales-submit-draft-0001".to_owned(),
            expected_engagement_generation: reloaded.engagement_generation,
            draft_id: saved.draft.draft_id.clone(),
            expected_draft_generation: saved.draft.draft_generation,
            expected_draft_hash: saved.draft.draft_hash.clone(),
            mapping_id: "synthetic.sales-document.engagement".to_owned(),
            version: "1.0.0".to_owned(),
            acknowledgement: OPERATOR_MAPPING_SUBMISSION_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("freeze immutable mapping version");
    assert_eq!(submitted.draft.state, "SUBMITTED");
    assert_eq!(
        submitted
            .coverage
            .required_canonical_fields_without_coverage,
        [] as [migration_stream::mapping::RequiredCanonicalFieldIdentity; 0]
    );
    assert_eq!(
        submitted.coverage.invalid_bindings,
        [] as [std::string::String; 0]
    );
    assert_eq!(submitted.coverage.mapped_canonical_fields.len(), 14);

    let coverage_before_approval = restarted
        .active_mapping_coverage(&PageRequest {
            page_size: 200,
            cursor: None,
        })
        .expect("submitted mapping coverage before approval");
    assert_eq!(
        coverage_before_approval.mapping.mapping_hash,
        submitted.mapping_hash
    );
    assert!(
        coverage_before_approval
            .items
            .iter()
            .all(|item| item.issue_code.is_none())
    );

    let restarted_again = sales_service(temporary.path());
    let reloaded_submitted = restarted_again
        .mapping_workspace()
        .expect("reload submitted mapping after restart");
    let durable_submitted = reloaded_submitted.draft.expect("submitted draft");
    assert_eq!(
        durable_submitted.submitted_mapping_hash.as_deref(),
        Some(submitted.mapping_hash.as_str())
    );
    let coverage_after_restart = restarted_again
        .active_mapping_coverage(&PageRequest {
            page_size: 200,
            cursor: None,
        })
        .expect("mapping coverage after restart");
    assert_eq!(
        coverage_after_restart.canonical_hash,
        coverage_before_approval.canonical_hash
    );

    let approval = restarted_again
        .approve_mapping_version(&ApproveMappingVersionCommand {
            schema_version: APPROVE_MAPPING_VERSION_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "sales-approve-mapping-0001".to_owned(),
            expected_engagement_generation: reloaded_submitted.engagement_generation,
            draft_id: durable_submitted.draft_id,
            expected_draft_generation: durable_submitted.draft_generation,
            expected_draft_hash: durable_submitted.draft_hash,
            expected_mapping_id: "synthetic.sales-document.engagement".to_owned(),
            expected_mapping_version: "1.0.0".to_owned(),
            expected_mapping_hash: submitted.mapping_hash.clone(),
            actor: "local-practitioner".to_owned(),
            reason: "Approve this exact governed two-entity mapping hash for local validation."
                .to_owned(),
            acknowledgement: OPERATOR_MAPPING_APPROVAL_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("approve exact immutable mapping hash");
    assert_eq!(approval.mapping_hash, submitted.mapping_hash);

    let validation = restarted_again
        .launch_validation(&LaunchValidationCommand {
            schema_version: LAUNCH_VALIDATION_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "sales-launch-validation-0001".to_owned(),
            expected_engagement_generation: restarted_again
                .state()
                .expect("validation state")
                .generation,
            expected_job_id: accepted.job_id,
            expected_mapping_id: approval.mapping_id,
            expected_mapping_version: approval.mapping_version,
            expected_mapping_hash: approval.mapping_hash.clone(),
        })
        .expect("launch entity-aware relationship-aware validation");
    assert_eq!(validation.status.rows_processed, 10);
    assert_eq!(validation.status.issue_count, 5);
    assert_eq!(
        restarted_again.state().expect("state").phase,
        EngagementPhase::FirstIssuesReady
    );

    let issues = ReadService::new(temporary.path())
        .issue_list(
            &validation.status.run_id,
            &IssueFilters::default(),
            &PageRequest {
                page_size: 200,
                cursor: None,
            },
        )
        .expect("read broad First Issues page");
    assert_eq!(issues.items.len(), 5);
    let codes = issues
        .items
        .iter()
        .map(|issue| issue.issue_code.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        codes,
        BTreeSet::from([
            "PATTERN_MISMATCH",
            "RELATIONSHIP_MINIMUM_CHILD_MISSING",
            "RELATIONSHIP_PARENT_NOT_FOUND",
            "UNIQUE_DUPLICATE",
        ])
    );
    assert_eq!(
        issues
            .items
            .iter()
            .filter(|issue| issue.issue_code == "UNIQUE_DUPLICATE")
            .count(),
        2
    );
    assert!(
        issues
            .items
            .iter()
            .any(|issue| issue.entity_id == HEADER_ENTITY)
    );
    assert!(
        issues
            .items
            .iter()
            .any(|issue| issue.entity_id == ITEM_ENTITY)
    );
    let broad_issue_json = serde_json::to_string(&issues).expect("serialize issue page");
    for received_value in ["SD-1001", "CUST-0042", "SKU-A", "PO-7781"] {
        assert!(
            !broad_issue_json.contains(received_value),
            "broad issue evidence leaked {received_value}"
        );
    }

    let context = restarted_again.context().expect("engagement context");
    assert!(!context.capabilities.governed_issue_decisions);
    assert!(
        restarted_again
            .multi_entity_authority_gated()
            .expect("gate")
    );
    let final_integrity = integrity_check(temporary.path()).expect("integrity replay");
    assert!(final_integrity.ok, "{:?}", final_integrity.issues);

    let database_path = temporary.path().join("state.sqlite3");
    let connection = rusqlite::Connection::open(&database_path).expect("tamper database");
    let validation_attempts_before: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_validation_attempts",
            [],
            |row| row.get(0),
        )
        .expect("validation attempt count before denied launch");
    let validation_runs_before: i64 = connection
        .query_row("SELECT COUNT(*) FROM validation_runs", [], |row| row.get(0))
        .expect("validation run count before denied launch");
    let canonical_approval_before: String = connection
        .query_row(
            "SELECT canonical_approval_json FROM engagement_mapping_approvals",
            [],
            |row| row.get(0),
        )
        .expect("canonical approval before tamper");
    let mut altered_approval: serde_json::Value =
        serde_json::from_str(&canonical_approval_before).expect("parse canonical approval");
    altered_approval["reason"] =
        serde_json::Value::String("Altered reason that was never approved.".to_owned());
    let altered_canonical_approval =
        canonical_json_string(&altered_approval).expect("canonicalize altered approval");
    connection
        .execute_batch("DROP TRIGGER engagement_mapping_approvals_no_update;")
        .expect("open semantic approval tamper window");
    connection
        .execute(
            "UPDATE engagement_mapping_approvals
             SET canonical_approval_json = ?1",
            [altered_canonical_approval],
        )
        .expect("write valid but unapproved canonical approval evidence");
    connection
        .execute_batch(
            "CREATE TRIGGER engagement_mapping_approvals_no_update
                 BEFORE UPDATE ON engagement_mapping_approvals
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'engagement mapping approvals are immutable'
                     );
                 END;",
        )
        .expect("restore approval guard after semantic tamper");
    drop(connection);

    let active = restarted_again
        .state()
        .expect("active state after validation");
    let denied = restarted_again
        .launch_validation(&LaunchValidationCommand {
            schema_version: LAUNCH_VALIDATION_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "sales-launch-after-approval-tamper".to_owned(),
            expected_engagement_generation: active.generation,
            expected_job_id: active.active.job_id.expect("active job"),
            expected_mapping_id: active.active.mapping_id.expect("active mapping"),
            expected_mapping_version: active
                .active
                .mapping_version
                .expect("active mapping version"),
            expected_mapping_hash: active.active.mapping_hash.expect("active mapping hash"),
        })
        .expect_err("corrupted approval evidence must not authorize validation");
    assert!(
        matches!(
            denied,
            KernelError::IntegrityFailed(ref detail)
                if detail.contains("mapping approval")
        ),
        "{denied:?}"
    );
    let connection =
        rusqlite::Connection::open(&database_path).expect("verify denied launch persistence");
    let validation_attempts_after: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM engagement_validation_attempts",
            [],
            |row| row.get(0),
        )
        .expect("validation attempt count after denied launch");
    let validation_runs_after: i64 = connection
        .query_row("SELECT COUNT(*) FROM validation_runs", [], |row| row.get(0))
        .expect("validation run count after denied launch");
    assert_eq!(validation_attempts_after, validation_attempts_before);
    assert_eq!(validation_runs_after, validation_runs_before);
    drop(connection);
    let approval_tamper_integrity =
        integrity_check(temporary.path()).expect("approval-tampered integrity replay");
    assert!(!approval_tamper_integrity.ok);
    assert!(
        approval_tamper_integrity
            .issues
            .iter()
            .any(|issue| issue.contains("mapping approval canonical hash mismatch")),
        "{:?}",
        approval_tamper_integrity.issues
    );
    let connection =
        rusqlite::Connection::open(&database_path).expect("approval bounds tamper database");
    connection
        .execute_batch(
            "DROP TRIGGER engagement_mapping_approvals_no_update;
             PRAGMA ignore_check_constraints = ON;",
        )
        .expect("open bounded approval tamper window");
    connection
        .execute(
            "UPDATE engagement_mapping_approvals
             SET canonical_approval_json = ?1",
            ["x".repeat(65_537)],
        )
        .expect("write over-bound approval evidence");
    connection
        .execute_batch(
            "PRAGMA ignore_check_constraints = OFF;
             CREATE TRIGGER engagement_mapping_approvals_no_update
                 BEFORE UPDATE ON engagement_mapping_approvals
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'engagement mapping approvals are immutable'
                     );
                 END;",
        )
        .expect("restore approval guard after bounded tamper");
    drop(connection);
    let approval_bounds_integrity =
        integrity_check(temporary.path()).expect("approval-bounds integrity replay");
    assert!(!approval_bounds_integrity.ok);
    assert!(
        approval_bounds_integrity.issues.iter().any(
            |issue| issue.contains("mapping approvals row(s) exceed canonical/metadata bounds")
        ),
        "{:?}",
        approval_bounds_integrity.issues
    );
    let connection =
        rusqlite::Connection::open(&database_path).expect("restore approval evidence database");
    connection
        .execute_batch("DROP TRIGGER engagement_mapping_approvals_no_update;")
        .expect("open approval restoration window");
    connection
        .execute(
            "UPDATE engagement_mapping_approvals
             SET canonical_approval_json = ?1",
            [canonical_approval_before],
        )
        .expect("restore exact canonical approval evidence");
    connection
        .execute_batch(
            "CREATE TRIGGER engagement_mapping_approvals_no_update
                 BEFORE UPDATE ON engagement_mapping_approvals
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'engagement mapping approvals are immutable'
                     );
                 END;",
        )
        .expect("restore approval immutability guard");
    drop(connection);
    let restored_integrity =
        integrity_check(temporary.path()).expect("restored approval integrity replay");
    assert!(restored_integrity.ok, "{:?}", restored_integrity.issues);

    let connection = rusqlite::Connection::open(&database_path).expect("link tamper database");
    connection
        .execute_batch(
            "DROP TRIGGER engagement_mapping_workspace_attempts_no_update;
             UPDATE engagement_mapping_workspace_attempts
             SET result_reference = 'wrong-draft-result'
             WHERE command_kind = 'SAVE_DRAFT';
             CREATE TRIGGER engagement_mapping_workspace_attempts_no_update
                 BEFORE UPDATE ON engagement_mapping_workspace_attempts
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'engagement mapping workspace attempts are immutable'
                     );
                 END;",
        )
        .expect("corrupt attempt result link behind restored guard");
    drop(connection);
    let linked_tamper_integrity =
        integrity_check(temporary.path()).expect("linked-tampered integrity replay");
    assert!(!linked_tamper_integrity.ok);
    assert!(
        linked_tamper_integrity
            .issues
            .iter()
            .any(|issue| issue.contains("SAVE_DRAFT attempt")),
        "{:?}",
        linked_tamper_integrity.issues
    );
    assert!(
        linked_tamper_integrity
            .issues
            .iter()
            .any(|issue| issue.contains("authoritative creation results; expected one")),
        "{:?}",
        linked_tamper_integrity.issues
    );

    let connection = rusqlite::Connection::open(&database_path).expect("draft tamper database");
    connection
        .execute_batch(
            "DROP TRIGGER engagement_mapping_draft_revisions_no_update;
             UPDATE engagement_mapping_draft_revisions
             SET canonical_draft_json = '{}'
             WHERE generation = 1;
             CREATE TRIGGER engagement_mapping_draft_revisions_no_update
                 BEFORE UPDATE ON engagement_mapping_draft_revisions
                 BEGIN
                     SELECT RAISE(
                         ABORT,
                         'engagement mapping draft revisions are immutable'
                     );
                 END;",
        )
        .expect("corrupt draft revision behind restored guard");
    drop(connection);
    let draft_tamper_integrity =
        integrity_check(temporary.path()).expect("draft-tampered integrity replay");
    assert!(!draft_tamper_integrity.ok);
    assert!(
        draft_tamper_integrity
            .issues
            .iter()
            .any(|issue| issue.contains("mapping draft revision")),
        "{:?}",
        draft_tamper_integrity.issues
    );

    let connection = rusqlite::Connection::open(&database_path).expect("guard tamper database");
    connection
        .execute_batch("DROP TRIGGER engagement_mapping_workspace_attempts_no_delete;")
        .expect("remove one immutability guard");
    drop(connection);
    let guard_tamper_integrity =
        integrity_check(temporary.path()).expect("guard-tampered integrity replay");
    assert!(!guard_tamper_integrity.ok);
    assert!(
        guard_tamper_integrity
            .issues
            .iter()
            .any(|issue| issue.contains("required immutability guard")),
        "{:?}",
        guard_tamper_integrity.issues
    );
}

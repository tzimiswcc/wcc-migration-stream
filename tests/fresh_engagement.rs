use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
};

use migration_stream::{
    engagement::{
        ACCEPT_SOURCE_COMMAND_SCHEMA, APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA,
        ASSISTED_MAPPING_ACKNOWLEDGEMENT, AcceptPreflightCommand, ApproveProfileCommand,
        CREATE_ENGAGEMENT_COMMAND_SCHEMA, CompletePreflightCommand, ControlledObjectPack,
        CreateOrReopenCommand, EngagementPhase, EngagementService, EngagementServiceConfig,
        LAUNCH_VALIDATION_COMMAND_SCHEMA, LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT,
        LaunchValidationCommand, MappingMode, ObjectPackMappingSuggestion,
        PREPARE_MAPPING_COMMAND_SCHEMA, PrepareMappingCommand, RESERVE_PREFLIGHT_COMMAND_SCHEMA,
        ReservePreflightCommand, SELECT_OBJECT_PACK_COMMAND_SCHEMA,
        SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT, SYNTHETIC_REVIEWED_OBJECT_PACK_ID,
        SelectObjectPackCommand, SourcePreflightState,
    },
    mapping::{MappingKind, ReferenceTable},
    policy::IntakePolicy,
    profiling::ProfilingPolicy,
    state::{SCHEMA_VERSION, integrity_check},
    validation::ValidationPolicy,
};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

fn repository_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn reference_tables() -> Vec<ReferenceTable> {
    vec![
        ReferenceTable {
            table_id: "synthetic-category-domain".to_owned(),
            version: "1.0.0".to_owned(),
            entity_id: None,
            values: BTreeMap::from([
                ("ORGANIZATION".to_owned(), "ORGANIZATION".to_owned()),
                ("PERSON".to_owned(), "PERSON".to_owned()),
            ]),
        },
        ReferenceTable {
            table_id: "synthetic-category-map".to_owned(),
            version: "1.0.0".to_owned(),
            entity_id: None,
            values: BTreeMap::from([
                ("1".to_owned(), "PERSON".to_owned()),
                ("2".to_owned(), "ORGANIZATION".to_owned()),
            ]),
        },
    ]
}

fn controlled_pack() -> ControlledObjectPack {
    ControlledObjectPack {
        object_pack_id: SYNTHETIC_REVIEWED_OBJECT_PACK_ID.to_owned(),
        version: "1.0.0".to_owned(),
        title: "Synthetic customer readiness proof pack".to_owned(),
        description: "Synthetic reviewed proof material only.".to_owned(),
        scope: "Synthetic local customer-readiness validation proof only.".to_owned(),
        root_entity_id: "Customer".to_owned(),
        mapping_mode: MappingMode::ControlledSuggestions,
        limitations: vec![
            "No external target-system compatibility claim.".to_owned(),
            "Local mapping/validation authority requires explicit approval.".to_owned(),
        ],
        mapping_suggestions: vec![
            ObjectPackMappingSuggestion {
                suggestion_id: "synthetic.customer.source-id".to_owned(),
                target_entity_id: None,
                target_field_id: "sourceCustomerId".to_owned(),
                source_file: Some("customer_accounts.csv".to_owned()),
                source_column: Some("migrationPartnerId".to_owned()),
                source_column_ordinal: Some(0),
                kind: MappingKind::Direct,
                constant: None,
                lookup_table_id: None,
            },
            ObjectPackMappingSuggestion {
                suggestion_id: "synthetic.customer.category".to_owned(),
                target_entity_id: None,
                target_field_id: "category".to_owned(),
                source_file: Some("customer_accounts.csv".to_owned()),
                source_column: Some("category".to_owned()),
                source_column_ordinal: Some(1),
                kind: MappingKind::Lookup,
                constant: None,
                lookup_table_id: Some("synthetic-category-map".to_owned()),
            },
            ObjectPackMappingSuggestion {
                suggestion_id: "synthetic.customer.name".to_owned(),
                target_entity_id: None,
                target_field_id: "name".to_owned(),
                source_file: Some("customer_accounts.csv".to_owned()),
                source_column: Some("organizationName1".to_owned()),
                source_column_ordinal: Some(2),
                kind: MappingKind::Trim,
                constant: None,
                lookup_table_id: None,
            },
            ObjectPackMappingSuggestion {
                suggestion_id: "synthetic.customer.notes".to_owned(),
                target_entity_id: None,
                target_field_id: "notes".to_owned(),
                source_file: Some("customer_accounts.csv".to_owned()),
                source_column: Some("notes".to_owned()),
                source_column_ordinal: Some(3),
                kind: MappingKind::EmptyAsNull,
                constant: None,
                lookup_table_id: None,
            },
            ObjectPackMappingSuggestion {
                suggestion_id: "synthetic.customer.duplicate-group".to_owned(),
                target_entity_id: None,
                target_field_id: "duplicateGroupKey".to_owned(),
                source_file: None,
                source_column: None,
                source_column_ordinal: None,
                kind: MappingKind::Constant,
                constant: Some("SYNTHETIC-DUPLICATE".to_owned()),
                lookup_table_id: None,
            },
        ],
        reference_tables: reference_tables(),
        profile_document: serde_json::from_slice(
            &fs::read(repository_path(
                "object-packs/synthetic.customer.readiness/1.0.0/synthetic-profile.reviewed.json",
            ))
            .expect("read controlled profile"),
        )
        .expect("parse controlled profile"),
    }
}

fn service(workspace: &Path) -> EngagementService {
    migration_stream::object_workspace::establish_for_local_automation(
        workspace,
        "Synthetic customer workspace",
        "Customer readiness",
        "Automated integration proof",
    )
    .expect("initialize object workspace");
    let mut profiling_policy =
        ProfilingPolicy::from_path(&repository_path("config/engagement-profiling-policy.json"))
            .expect("profiling policy");
    // Exercise the setup boundary itself: even a permissive caller policy must
    // become sample-free before any engagement evidence is produced.
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
        object_packs: vec![controlled_pack()],
    })
    .expect("engagement service")
}

fn file_identity(path: &Path) -> (String, u64) {
    let bytes = fs::read(path).expect("fixture bytes");
    (
        migration_stream::canonical::lower_hex(Sha256::digest(&bytes)),
        u64::try_from(bytes.len()).expect("fixture size"),
    )
}

fn assert_shared_retained_publication_authority(workspace: &Path) {
    let connection =
        Connection::open(workspace.join("state.sqlite3")).expect("open authority database");
    let publish_intents = connection
        .prepare(
            "SELECT detail FROM engagement_preflight_events
             WHERE detail IS NOT NULL ORDER BY sequence",
        )
        .expect("prepare publication authority query")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query publication authorities")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect publication authorities")
        .into_iter()
        .filter_map(|detail| {
            let value = serde_json::from_str::<serde_json::Value>(&detail).ok()?;
            (value["schemaVersion"] == "migration-stream/engagement/preflight-publish-intent/v2")
                .then_some(value)
        })
        .collect::<Vec<_>>();
    assert_eq!(publish_intents.len(), 2);
    assert_ne!(
        publish_intents[0]["partialFileIdentity"], publish_intents[1]["partialFileIdentity"],
        "each reservation keeps its own upload-part identity"
    );
    assert_eq!(
        publish_intents[0]["retainedFileIdentity"], publish_intents[1]["retainedFileIdentity"],
        "identical packages share only the governed retained identity"
    );
}

fn create_engagement(service: &EngagementService) {
    service
        .create_or_reopen(&CreateOrReopenCommand {
            schema_version: CREATE_ENGAGEMENT_COMMAND_SCHEMA.to_owned(),
            display_name: "Fresh customer readiness proof".to_owned(),
            idempotency_key: "create-engagement-0001".to_owned(),
        })
        .expect("create engagement");
}

fn ready_preflight_workspace(label: &str) -> (TempDir, String, PathBuf) {
    let temporary = TempDir::new().expect("temporary workspace");
    let service = service(temporary.path());
    create_engagement(&service);
    let fixture = repository_path("fixtures/packages/valid.zip");
    let (source_hash, source_size) = file_identity(&fixture);
    let reservation = service
        .reserve_preflight(&ReservePreflightCommand {
            schema_version: RESERVE_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            source_name: format!("{label}.zip"),
            idempotency_key: format!("reserve-{label}-0001"),
        })
        .expect("reserve source");
    fs::copy(&fixture, &reservation.controlled_path).expect("stream fixture");
    let preflight_id = reservation.preflight.preflight_id;
    service
        .complete_preflight(&CompletePreflightCommand {
            preflight_id: preflight_id.clone(),
            sha256: source_hash.clone(),
            size_bytes: source_size,
        })
        .expect("complete source preflight");
    drop(service);
    let retained = temporary
        .path()
        .join(format!("engagement/preflights/retained/{source_hash}.zip"));
    let integrity = integrity_check(temporary.path()).expect("clean preflight integrity");
    assert!(integrity.ok, "{:?}", integrity.issues);
    (temporary, preflight_id, retained)
}

fn load_v2_publish_event(connection: &Connection, preflight_id: &str) -> (i64, serde_json::Value) {
    let (sequence, detail): (i64, String) = connection
        .query_row(
            "SELECT sequence, detail FROM engagement_preflight_events
             WHERE preflight_id = ?1
               AND detail LIKE '%preflight-publish-intent/v2%'
             ORDER BY sequence DESC LIMIT 1",
            [preflight_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("load v2 publication authority");
    (
        sequence,
        serde_json::from_str(&detail).expect("parse v2 publication authority"),
    )
}

fn assert_preflight_integrity_rejected(workspace: &Path, expected: &str) {
    let report = integrity_check(workspace).expect("tampered preflight integrity");
    assert!(!report.ok, "tamper unexpectedly passed global integrity");
    assert!(
        report.issues.iter().any(
            |issue| issue.contains("filesystem authority mismatch") && issue.contains(expected)
        ),
        "{:?}",
        report.issues
    );
}

#[test]
fn global_integrity_rejects_altered_v2_retained_identity() {
    let (temporary, preflight_id, _retained) =
        ready_preflight_workspace("retained-identity-tamper");
    let connection =
        Connection::open(temporary.path().join("state.sqlite3")).expect("open database");
    let (sequence, mut authority) = load_v2_publish_event(&connection, &preflight_id);
    let original = authority["retainedFileIdentity"]["secondary"]
        .as_u64()
        .expect("retained secondary identity");
    authority["retainedFileIdentity"]["secondary"] = serde_json::json!(original.saturating_add(1));
    let altered = migration_stream::canonical::canonical_json_string(&authority)
        .expect("canonical altered authority");
    connection
        .execute_batch("DROP TRIGGER engagement_preflight_events_no_update;")
        .expect("open immutable history for tamper simulation");
    connection
        .execute(
            "UPDATE engagement_preflight_events SET detail = ?1 WHERE sequence = ?2",
            params![altered, sequence],
        )
        .expect("alter retained identity");
    drop(connection);

    assert_preflight_integrity_rejected(temporary.path(), "file identity");
}

#[test]
fn global_integrity_rejects_same_byte_retained_inode_replacement() {
    let (temporary, _preflight_id, retained) =
        ready_preflight_workspace("retained-inode-replacement");
    let displaced = retained.with_extension("displaced");
    fs::rename(&retained, &displaced).expect("displace governed retained inode");
    fs::copy(&displaced, &retained).expect("replace with byte-identical inode");
    let mut permissions = fs::metadata(&retained)
        .expect("replacement metadata")
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&retained, permissions).expect("freeze replacement");
    fs::remove_file(displaced).expect("remove displaced inode");

    assert_preflight_integrity_rejected(temporary.path(), "file identity");
}

#[test]
fn global_integrity_rejects_v1_and_v2_publication_authority_conflict() {
    let (temporary, preflight_id, _retained) =
        ready_preflight_workspace("publication-schema-conflict");
    let connection =
        Connection::open(temporary.path().join("state.sqlite3")).expect("open database");
    let (_, mut legacy) = load_v2_publish_event(&connection, &preflight_id);
    legacy["schemaVersion"] =
        serde_json::json!("migration-stream/engagement/preflight-publish-intent/v1");
    legacy
        .as_object_mut()
        .expect("publication authority object")
        .remove("retainedFileIdentity");
    let legacy =
        migration_stream::canonical::canonical_json_string(&legacy).expect("canonical v1 intent");
    connection
        .execute(
            "INSERT INTO engagement_preflight_events (
                 preflight_id, state, error_code, detail, occurred_at
             ) VALUES (?1, 'READY', NULL, ?2, '2026-07-29T00:00:00.000Z')",
            params![preflight_id, legacy],
        )
        .expect("inject conflicting v1 authority");
    drop(connection);

    assert_preflight_integrity_rejected(temporary.path(), "schema generations");
}

#[test]
fn global_integrity_rejects_undeclared_retained_acceptance_alias() {
    let (temporary, _preflight_id, retained) = ready_preflight_workspace("retained-alias-tamper");
    let acceptance_directory = temporary.path().join("engagement/acceptances");
    fs::create_dir_all(&acceptance_directory).expect("create controlled acceptance namespace");
    fs::hard_link(
        &retained,
        acceptance_directory.join("undeclared-retained-alias.zip"),
    )
    .expect("inject undeclared acceptance alias");

    assert_preflight_integrity_rejected(temporary.path(), "hard-link alias");
}

#[test]
fn global_integrity_rejects_altered_preflight_history_trigger() {
    let (temporary, _preflight_id, _retained) =
        ready_preflight_workspace("preflight-history-trigger-tamper");
    let connection =
        Connection::open(temporary.path().join("state.sqlite3")).expect("open database");
    connection
        .execute_batch(
            "DROP TRIGGER engagement_preflight_events_no_delete;
             CREATE TRIGGER engagement_preflight_events_no_delete
             BEFORE DELETE ON engagement_preflight_events
             BEGIN
                 SELECT RAISE(ABORT, 'different immutable-history guard');
             END;",
        )
        .expect("alter immutable-history trigger");
    drop(connection);

    let report = integrity_check(temporary.path()).expect("trigger-tampered integrity");
    assert!(!report.ok);
    assert!(
        report.issues.iter().any(|issue| {
            issue.contains("engagement_preflight_events_no_delete")
                && issue.contains("missing or altered")
        }),
        "{:?}",
        report.issues
    );
}

#[test]
fn empty_active_workspace_integrity_replays_preflight_history_guards() {
    for trigger in [
        "engagement_preflight_events_no_update",
        "engagement_preflight_events_no_delete",
    ] {
        let temporary = TempDir::new().expect("temporary workspace");
        migration_stream::object_workspace::establish_for_local_automation(
            temporary.path(),
            "Empty integrity workspace",
            "Synthetic empty object",
            "Automated integration proof",
        )
        .expect("initialize empty object workspace");
        let connection =
            Connection::open(temporary.path().join("state.sqlite3")).expect("open database");
        connection
            .execute_batch(&format!("DROP TRIGGER {trigger};"))
            .expect("remove exact preflight history guard");
        drop(connection);

        let report = integrity_check(temporary.path()).expect("empty-workspace integrity");
        assert!(!report.ok);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.contains(trigger) && issue.contains("missing or altered")),
            "{trigger}: {:?}",
            report.issues
        );
    }
}

#[test]
fn global_integrity_rejects_altered_validation_attempt_guard() {
    let temporary = TempDir::new().expect("temporary workspace");
    let _service = service(temporary.path());
    let database =
        Connection::open(temporary.path().join("state.sqlite3")).expect("workspace database");
    database
        .execute_batch(
            "DROP TRIGGER engagement_validation_attempts_no_delete;
             CREATE TRIGGER engagement_validation_attempts_no_delete
             BEFORE DELETE ON engagement_validation_attempts
             BEGIN SELECT 1; END;",
        )
        .expect("replace validation-attempt guard");
    drop(database);

    let integrity = integrity_check(temporary.path()).expect("integrity report");
    assert!(!integrity.ok);
    assert!(
        integrity
            .issues
            .iter()
            .any(|issue| issue.contains("validation-attempt guard"))
    );
}

#[test]
fn global_integrity_accepts_legacy_v1_single_publication_authority() {
    let (temporary, preflight_id, _retained) =
        ready_preflight_workspace("legacy-single-publication");
    let connection =
        Connection::open(temporary.path().join("state.sqlite3")).expect("open database");
    let (sequence, mut legacy) = load_v2_publish_event(&connection, &preflight_id);
    legacy["schemaVersion"] =
        serde_json::json!("migration-stream/engagement/preflight-publish-intent/v1");
    legacy
        .as_object_mut()
        .expect("publication authority object")
        .remove("retainedFileIdentity");
    let legacy =
        migration_stream::canonical::canonical_json_string(&legacy).expect("canonical v1 intent");
    connection
        .execute_batch("DROP TRIGGER engagement_preflight_events_no_update;")
        .expect("open immutable history for legacy-fixture simulation");
    connection
        .execute(
            "UPDATE engagement_preflight_events SET detail = ?1 WHERE sequence = ?2",
            params![legacy, sequence],
        )
        .expect("install legacy single-publication authority");
    connection
        .execute_batch(
            "CREATE TRIGGER engagement_preflight_events_no_update
             BEFORE UPDATE ON engagement_preflight_events
             BEGIN
                 SELECT RAISE(ABORT, 'engagement preflight history is immutable');
             END;",
        )
        .expect("restore exact immutable-history trigger");
    drop(connection);

    let report = integrity_check(temporary.path()).expect("legacy v1 global integrity");
    assert!(report.ok, "{:?}", report.issues);
    let reopened = service(temporary.path());
    assert_eq!(
        reopened
            .detail()
            .expect("reopen legacy single publication")
            .preflights[0]
            .state,
        SourcePreflightState::Ready
    );
}

#[test]
fn pre_reserved_original_source_cannot_replace_the_accepted_active_source() {
    let temporary = TempDir::new().expect("temporary workspace");
    let service = service(temporary.path());
    create_engagement(&service);
    let fixture = repository_path("fixtures/packages/valid.zip");
    let (source_hash, source_size) = file_identity(&fixture);
    let mut ready = Vec::new();
    for (source_name, idempotency_key) in [
        ("first.zip", "reserve-first-source"),
        ("replacement.zip", "reserve-replacement"),
    ] {
        let reservation = service
            .reserve_preflight(&ReservePreflightCommand {
                schema_version: RESERVE_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
                source_name: source_name.to_owned(),
                idempotency_key: idempotency_key.to_owned(),
            })
            .expect("reserve source before activation");
        fs::copy(&fixture, &reservation.controlled_path).expect("stream fixture");
        ready.push(
            service
                .complete_preflight(&CompletePreflightCommand {
                    preflight_id: reservation.preflight.preflight_id,
                    sha256: source_hash.clone(),
                    size_bytes: source_size,
                })
                .expect("complete source preflight"),
        );
    }
    assert_shared_retained_publication_authority(temporary.path());
    let first_command = AcceptPreflightCommand {
        schema_version: ACCEPT_SOURCE_COMMAND_SCHEMA.to_owned(),
        preflight_id: ready[0].preflight_id.clone(),
        expected_source_hash: source_hash.clone(),
        expected_manifest_hash: ready[0].manifest_hash.clone().expect("first manifest"),
        idempotency_key: "accept-first-source".to_owned(),
        acknowledgement: SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT.to_owned(),
    };
    let accepted = service
        .accept_preflight(&first_command)
        .expect("accept first source");
    let before = service.state().expect("active source state");
    let replacement = service
        .accept_preflight(&AcceptPreflightCommand {
            schema_version: ACCEPT_SOURCE_COMMAND_SCHEMA.to_owned(),
            preflight_id: ready[1].preflight_id.clone(),
            expected_source_hash: source_hash,
            expected_manifest_hash: ready[1]
                .manifest_hash
                .clone()
                .expect("replacement manifest"),
            idempotency_key: "accept-replacement".to_owned(),
            acknowledgement: SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect_err("a second original source must not replace active evidence");
    assert!(matches!(
        replacement,
        migration_stream::KernelError::Conflict(_)
    ));
    assert_eq!(service.state().expect("unchanged active source"), before);
    assert_eq!(
        service
            .accept_preflight(&first_command)
            .expect("unchanged idempotent acceptance")
            .job_id,
        accepted.job_id
    );
    let detail = service.detail().expect("preflight evidence");
    assert_eq!(
        detail
            .preflights
            .iter()
            .find(|preflight| preflight.preflight_id == ready[1].preflight_id)
            .expect("replacement preflight remains visible")
            .state,
        SourcePreflightState::Ready
    );
    drop(service);
    let reopened = crate::service(temporary.path());
    assert_eq!(
        reopened
            .detail()
            .expect("restart shared retained evidence")
            .preflights
            .iter()
            .filter(|preflight| preflight.state == SourcePreflightState::Ready)
            .count(),
        2
    );
    let integrity = integrity_check(temporary.path()).expect("shared retained global integrity");
    assert!(integrity.ok, "{:?}", integrity.issues);
}

#[test]
#[allow(clippy::too_many_lines)]
fn fresh_engagement_reaches_first_issues_with_explicit_authority() {
    let temporary = TempDir::new().expect("temporary workspace");
    let service = service(temporary.path());
    create_engagement(&service);

    let reservation = service
        .reserve_preflight(&ReservePreflightCommand {
            schema_version: RESERVE_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            source_name: "valid.zip".to_owned(),
            idempotency_key: "reserve-preflight-0001".to_owned(),
        })
        .expect("reserve source");
    let fixture = repository_path("fixtures/packages/valid.zip");
    fs::copy(&fixture, &reservation.controlled_path).expect("stream fixture bytes");
    let (source_hash, source_size) = file_identity(&fixture);
    let preflight = service
        .complete_preflight(&CompletePreflightCommand {
            preflight_id: reservation.preflight.preflight_id.clone(),
            sha256: source_hash.clone(),
            size_bytes: source_size,
        })
        .expect("complete preflight");
    assert_eq!(preflight.state, SourcePreflightState::Ready);
    let manifest_hash = preflight.manifest_hash.clone().expect("manifest hash");

    let accepted = service
        .accept_preflight(&AcceptPreflightCommand {
            schema_version: ACCEPT_SOURCE_COMMAND_SCHEMA.to_owned(),
            preflight_id: preflight.preflight_id.clone(),
            expected_source_hash: source_hash,
            expected_manifest_hash: manifest_hash,
            idempotency_key: "accept-preflight-0001".to_owned(),
            acknowledgement: SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("accept and profile");
    assert_ne!(accepted.job_id, "");
    assert_ne!(accepted.profiling_run_id, "");

    let database = Connection::open(temporary.path().join("state.sqlite3")).expect("database");
    let version: i64 = database
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("schema version");
    assert_eq!(version, SCHEMA_VERSION);
    let duplicate_semantics: Vec<String> = database
        .prepare("SELECT duplicate_keys_json FROM profiling_files WHERE run_id = ?1")
        .expect("profile files")
        .query_map([&accepted.profiling_run_id], |row| row.get(0))
        .expect("duplicate rows")
        .collect::<Result<_, _>>()
        .expect("duplicate values");
    assert!(duplicate_semantics.iter().all(|value| value == "[]"));
    let samples: Vec<String> = database
        .prepare("SELECT samples_json FROM profiling_columns WHERE run_id = ?1")
        .expect("profile samples")
        .query_map([&accepted.profiling_run_id], |row| row.get(0))
        .expect("sample rows")
        .collect::<Result<_, _>>()
        .expect("sample values");
    assert!(samples.iter().all(|samples| samples == "[]"));
    let (policy_json, result_json): (String, String) = database
        .query_row(
            "SELECT policy_json, result_json
             FROM profiling_runs WHERE id = ?1 AND state = 'COMPLETE'",
            [&accepted.profiling_run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("complete sample-free profiling evidence");
    let policy: serde_json::Value =
        serde_json::from_str(&policy_json).expect("stored profiling policy");
    assert_eq!(policy["maximum_samples_per_column"], 0);
    assert_eq!(policy["maximum_sample_value_bytes"], 0);
    assert!(!result_json.contains("\"sha256\""));
    assert!(!result_json.contains("\"value\""));
    drop(database);

    let selected = service
        .select_object_pack(&SelectObjectPackCommand {
            schema_version: SELECT_OBJECT_PACK_COMMAND_SCHEMA.to_owned(),
            object_pack_id: SYNTHETIC_REVIEWED_OBJECT_PACK_ID.to_owned(),
            idempotency_key: "select-object-pack-0001".to_owned(),
        })
        .expect("select reviewed pack");
    assert_eq!(
        selected.profile.lifecycle_status,
        migration_stream::profile_registry::ProfileLifecycleStatus::ReviewedDraft
    );
    assert_eq!(
        selected.profile.authority,
        migration_stream::profile_registry::ProfileAuthority::None
    );
    service
        .approve_profile(&ApproveProfileCommand {
            schema_version: APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA.to_owned(),
            actor: "local-practitioner".to_owned(),
            reason: "Approve this reviewed synthetic proof profile locally.".to_owned(),
            authority_acknowledgement: LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT
                .to_owned(),
            idempotency_key: "approve-profile-0001".to_owned(),
        })
        .expect("explicit approval");
    assert_eq!(
        service.journey().expect("assisted mapping journey").stages[3]
            .href
            .as_deref(),
        Some("/mapping")
    );

    let source_profile = service
        .source_profile_rules()
        .expect("source/profile/rules");
    assert!(!source_profile.stored_sample_values);
    assert_eq!(source_profile.source_columns.len(), 4);
    assert_eq!(source_profile.profile_fields.len(), 5);
    assert_eq!(source_profile.rules.len(), 7);

    let pack = controlled_pack();
    let selected_suggestion_ids = pack
        .mapping_suggestions
        .iter()
        .map(|suggestion| suggestion.suggestion_id.clone())
        .collect();
    let prepared = service
        .prepare_mapping(&PrepareMappingCommand {
            schema_version: PREPARE_MAPPING_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "prepare-mapping-0001".to_owned(),
            mapping_id: "synthetic.customer.engagement".to_owned(),
            version: "1.0.0".to_owned(),
            selected_suggestion_ids,
            author: Some("local-practitioner".to_owned()),
            reviewer: Some("synthetic reviewed pack".to_owned()),
            acknowledgement: ASSISTED_MAPPING_ACKNOWLEDGEMENT.to_owned(),
        })
        .expect("prepare and validate mapping");
    assert!(prepared.validation.is_some());

    let validation_command = LaunchValidationCommand {
        schema_version: LAUNCH_VALIDATION_COMMAND_SCHEMA.to_owned(),
        idempotency_key: "launch-validation-0001".to_owned(),
        expected_engagement_generation: service.state().expect("validation state").generation,
        expected_job_id: accepted.job_id.clone(),
        expected_mapping_id: prepared.document.mapping_id.clone(),
        expected_mapping_version: prepared.document.version.clone(),
        expected_mapping_hash: prepared.mapping_hash.clone(),
    };
    let mut competing_command = validation_command.clone();
    competing_command.idempotency_key = "launch-validation-competing".to_owned();
    let first_runner = self::service(temporary.path());
    let second_runner = self::service(temporary.path());
    let barrier = Arc::new(Barrier::new(2));
    let ((first_command, first_result), (second_command, second_result)) = thread::scope(|scope| {
        let first_barrier = Arc::clone(&barrier);
        let first = scope.spawn(move || {
            first_barrier.wait();
            let result = first_runner.launch_validation(&validation_command);
            (validation_command, result)
        });
        let second_barrier = Arc::clone(&barrier);
        let second = scope.spawn(move || {
            second_barrier.wait();
            let result = second_runner.launch_validation(&competing_command);
            (competing_command, result)
        });
        (
            first.join().expect("first launch thread"),
            second.join().expect("second launch thread"),
        )
    });
    let mut winning_command = None;
    let mut losing_command = None;
    let mut validation = None;
    let mut rejected_count = 0;
    for (command, result) in [
        (first_command, first_result),
        (second_command, second_result),
    ] {
        match result {
            Ok(result) => {
                winning_command = Some(command);
                validation = Some(result);
            }
            Err(
                migration_stream::KernelError::Conflict(_)
                | migration_stream::KernelError::WorkspaceLocked(_),
            ) => {
                losing_command = Some(command);
                rejected_count += 1;
            }
            Err(error) => panic!("unexpected competing launch error: {error:?}"),
        }
    }
    assert_eq!(rejected_count, 1);
    let validation = validation.expect("one exact validation launch");
    let winning_command = winning_command.expect("winning validation command");
    let losing_command = losing_command.expect("rejected validation command");
    assert_eq!(validation.status.issue_count, 5);
    assert!(matches!(
        service.launch_validation(&losing_command),
        Err(migration_stream::KernelError::Conflict(_))
    ));
    let retry = service
        .launch_validation(&winning_command)
        .expect("idempotent exact validation retry");
    assert!(retry.reused_attempt);
    assert_eq!(retry.status.run_id, validation.status.run_id);
    let database = Connection::open(temporary.path().join("state.sqlite3")).expect("database");
    let exact_attempt: (String, String, String, String, String, Option<String>) = database
        .query_row(
            "SELECT authority_schema, attempt_state, planned_run_id, run_id,
                    mapping_mode, mapping_approval_id
             FROM engagement_validation_attempts",
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
        .expect("exact validation attempt");
    assert_eq!(
        exact_attempt,
        (
            "EXACT_V13".to_owned(),
            "COMPLETE".to_owned(),
            validation.status.run_id.clone(),
            validation.status.run_id.clone(),
            "CONTROLLED_SUGGESTIONS".to_owned(),
            None,
        )
    );
    assert!(
        database
            .execute(
                "UPDATE engagement_validation_attempts SET authority_hash = ?1",
                ["0".repeat(64)],
            )
            .is_err(),
        "exact validation authority must be immutable"
    );
    let attempts_before: i64 = database
        .query_row(
            "SELECT COUNT(*) FROM engagement_validation_attempts",
            [],
            |row| row.get(0),
        )
        .expect("attempt count");
    assert_eq!(attempts_before, 1);
    let run_count: i64 = database
        .query_row("SELECT COUNT(*) FROM validation_runs", [], |row| row.get(0))
        .expect("validation run count");
    assert_eq!(run_count, 1);
    drop(database);
    assert_eq!(
        service.state().expect("state").phase,
        EngagementPhase::FirstIssuesReady
    );
    assert!(
        service
            .context()
            .expect("context")
            .capabilities
            .governed_issue_decisions
    );

    let integrity = integrity_check(temporary.path()).expect("integrity");
    assert!(integrity.ok, "{:?}", integrity.issues);

    let reopened = service
        .create_or_reopen(&CreateOrReopenCommand {
            schema_version: CREATE_ENGAGEMENT_COMMAND_SCHEMA.to_owned(),
            display_name: "Fresh customer readiness proof".to_owned(),
            idempotency_key: "reopen-engagement-0001".to_owned(),
        })
        .expect("reopen engagement");
    assert!(reopened.reopened);
    assert_eq!(
        service
            .state()
            .expect("reopened state")
            .active
            .validation_run_id,
        Some(validation.status.run_id)
    );

    let database = Connection::open(temporary.path().join("state.sqlite3")).expect("database");
    database
        .execute(
            "UPDATE engagement_active_context
             SET active_mapping_hash = ?1",
            ["0".repeat(64)],
        )
        .expect("tamper active mapping binding");
    drop(database);
    assert!(matches!(
        service.journey(),
        Err(migration_stream::KernelError::IntegrityFailed(_))
    ));
    assert!(
        !integrity_check(temporary.path())
            .expect("mapping-tampered integrity")
            .ok
    );

    let database = Connection::open(temporary.path().join("state.sqlite3")).expect("database");
    database
        .execute(
            "UPDATE engagement_active_context SET active_mapping_hash = ?1",
            [&prepared.mapping_hash],
        )
        .expect("restore mapping binding");
    database
        .execute(
            "UPDATE profiling_runs SET source_hash = ?1 WHERE id = ?2",
            params!["f".repeat(64), &accepted.profiling_run_id],
        )
        .expect("tamper profiling source binding");
    drop(database);
    assert!(matches!(
        service.journey(),
        Err(migration_stream::KernelError::IntegrityFailed(_))
    ));
    assert!(
        !integrity_check(temporary.path())
            .expect("profiling-tampered integrity")
            .ok
    );

    let database = Connection::open(temporary.path().join("state.sqlite3")).expect("database");
    database
        .execute(
            "UPDATE profiling_runs SET source_hash = ?1 WHERE id = ?2",
            params![&accepted.source_hash, &accepted.profiling_run_id],
        )
        .expect("restore profiling source binding");
    let manifest_json: String = database
        .query_row(
            "SELECT manifest_json FROM engagement_source_preflights
             WHERE preflight_id = ?1",
            [&preflight.preflight_id],
            |row| row.get(0),
        )
        .expect("accepted manifest document");
    let duplicate_manifest_json = format!(
        "{{\"csv_paths\":[],{}",
        manifest_json
            .strip_prefix('{')
            .expect("canonical manifest object")
    );
    database
        .execute(
            "UPDATE engagement_source_preflights
             SET manifest_json = ?1
             WHERE preflight_id = ?2",
            params![duplicate_manifest_json, &preflight.preflight_id],
        )
        .expect("tamper accepted manifest with a duplicate key");
    drop(database);
    assert!(matches!(
        service.journey(),
        Err(migration_stream::KernelError::IntegrityFailed(_))
    ));
    assert!(
        !integrity_check(temporary.path())
            .expect("manifest-tampered integrity")
            .ok
    );

    let database = Connection::open(temporary.path().join("state.sqlite3")).expect("database");
    database
        .execute(
            "UPDATE engagement_source_preflights
             SET manifest_json = ?1 WHERE preflight_id = ?2",
            params![manifest_json, &preflight.preflight_id],
        )
        .expect("restore accepted manifest document");
    database
        .pragma_update(None, "foreign_keys", "OFF")
        .expect("disable foreign keys for tamper simulation");
    database
        .execute(
            "UPDATE engagement_active_context
             SET active_profiling_run_id = 'missing-profiling-run'",
            [],
        )
        .expect("tamper profiling pointer");
    drop(database);
    assert!(matches!(
        service.journey(),
        Err(migration_stream::KernelError::IntegrityFailed(_))
    ));
}

#[test]
fn recovery_interrupts_only_reserved_preflight() {
    let temporary = TempDir::new().expect("temporary workspace");
    let service = service(temporary.path());
    create_engagement(&service);
    let reservation = service
        .reserve_preflight(&ReservePreflightCommand {
            schema_version: RESERVE_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            source_name: "interrupted.zip".to_owned(),
            idempotency_key: "reserve-preflight-0002".to_owned(),
        })
        .expect("reserve source");
    fs::write(&reservation.controlled_path, b"partial").expect("partial bytes");

    service.recover().expect("recover engagement");
    assert!(!reservation.controlled_path.exists());
    let detail = service.detail().expect("detail");
    assert_eq!(detail.preflights.len(), 1);
    assert_eq!(
        detail.preflights[0].state,
        SourcePreflightState::Interrupted
    );
}

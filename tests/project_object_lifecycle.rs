//! Synthetic linkage proof through the accepted envelope, existing engagement
//! authority and real CLI reads. Child drivers are test-only, never app launchers.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use migration_stream::{
    canonical::{canonical_hash, sha256_hex},
    engagement::{
        ACCEPT_SOURCE_COMMAND_SCHEMA, APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA,
        ASSISTED_MAPPING_ACKNOWLEDGEMENT, AcceptPreflightCommand, ApproveProfileCommand,
        CREATE_ENGAGEMENT_COMMAND_SCHEMA, CompletePreflightCommand, ControlledObjectPack,
        CreateOrReopenCommand, EngagementService, EngagementServiceConfig,
        LAUNCH_VALIDATION_COMMAND_SCHEMA, LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT,
        LaunchValidationCommand, PREPARE_MAPPING_COMMAND_SCHEMA, PrepareMappingCommand,
        RESERVE_PREFLIGHT_COMMAND_SCHEMA, ReservePreflightCommand,
        SELECT_OBJECT_PACK_COMMAND_SCHEMA, SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT,
        SelectObjectPackCommand, SourcePreflightState,
    },
    object_workspace::{ObjectWorkspaceService, ObjectWorkspaceState},
    policy::IntakePolicy,
    profile_registry::{ProfileAuthority, ProfileLifecycleStatus},
    profiling::ProfilingPolicy,
    project_object::{
        PENDING_PROJECT_FILENAME, PROJECT_FILENAME, bind_project, create_project, open_project,
    },
    validation::{ValidationPolicy, ValidationRunState},
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

const PROFILE_ID: &str = "synthetic.customer.readiness";
const PROFILE_SCHEMA: &str = "schemas/migration-object-profile.schema.json";

fn repository_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn cli(arguments: &[&str], workspace: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_migration-stream"));
    command.args(arguments).arg("--workspace").arg(workspace);
    command
}

fn success(command: &mut Command) -> Value {
    let output = command.output().expect("run real CLI child");
    assert!(
        output.status.success(),
        "CLI failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).expect("CLI success JSON");
    assert_eq!(response["ok"], true);
    response["result"].clone()
}

fn initialize_explicitly(workspace: &Path) {
    success(cli(&["object-workspace", "initialize"], workspace).args([
        "--display-name",
        "Synthetic lifecycle object",
        "--migration-object-name-or-scope",
        "Synthetic local customer readiness",
        "--creator-attribution",
        "Lifecycle proof operator",
    ]));
}

fn controlled_pack() -> ControlledObjectPack {
    let root = repository_path("object-packs/synthetic.customer.readiness/1.0.0");
    let manifest: Value =
        serde_json::from_slice(&fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let profile: Value = serde_json::from_slice(
        &fs::read(root.join(manifest["profileFile"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    assert_eq!(canonical_hash(&profile).unwrap(), manifest["profileSha256"]);
    let mut pack: ControlledObjectPack = serde_json::from_value(json!({
        "objectPackId": manifest["packId"], "version": manifest["version"],
        "title": manifest["title"], "description": manifest["scope"], "scope": manifest["scope"],
        "rootEntityId": manifest["rootEntityId"], "mappingMode": "CONTROLLED_SUGGESTIONS",
        "limitations": manifest["limitations"], "mappingSuggestions": manifest["mappingSuggestions"],
        "referenceTables": manifest["referenceTables"],
    })).unwrap();
    pack.profile_document = profile;
    pack
}

fn service(workspace: &Path) -> EngagementService {
    EngagementService::new(EngagementServiceConfig {
        workspace: workspace.to_path_buf(),
        intake_policy: IntakePolicy::from_path(Some(&repository_path(
            "config/engagement-intake-policy.json",
        )))
        .unwrap(),
        profiling_policy: ProfilingPolicy::from_path(&repository_path(
            "config/engagement-profiling-policy.json",
        ))
        .unwrap(),
        profile_schema_path: repository_path(PROFILE_SCHEMA),
        mapping_schema_path: repository_path("schemas/source-mapping-version.schema.json"),
        validation_policy: ValidationPolicy::from_path(&repository_path(
            "fixtures/wp003/validation-policy.json",
        ))
        .unwrap(),
        object_packs: vec![controlled_pack()],
    })
    .unwrap()
}

fn immutable_evidence(workspace: &Path) -> BTreeMap<String, Value> {
    fn walk(root: &Path, directory: &Path, files: &mut BTreeMap<String, Value>) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, files);
            } else {
                assert!(entry.file_type().unwrap().is_file());
                let bytes = fs::read(&path).unwrap();
                assert!(
                    !bytes.is_empty(),
                    "immutable evidence must contain bytes: {}",
                    path.display()
                );
                assert!(entry.metadata().unwrap().permissions().readonly());
                files.insert(
                    // Evidence keys are portable relative names, not native paths.
                    path.strip_prefix(root)
                        .unwrap()
                        .components()
                        .map(|part| part.as_os_str().to_str().unwrap())
                        .collect::<Vec<_>>()
                        .join("/"),
                    json!({"sha256": sha256_hex(&bytes), "bytes": bytes.len()}),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    for relative in [
        "packages",
        "engagement/preflights/retained",
        "engagement/acceptances",
    ] {
        let directory = workspace.join(relative);
        assert!(
            directory.is_dir(),
            "required immutable-evidence directory is missing: {relative}"
        );
        let before = files.len();
        walk(workspace, &directory, &mut files);
        assert!(
            files.len() > before,
            "required immutable-evidence directory is empty: {relative}"
        );
    }
    files
}

fn durable_snapshot(workspace: &Path) -> Value {
    let authority = ObjectWorkspaceService::new(workspace).unwrap();
    let workspace = authority.root();
    let engagement = service(workspace);
    let state = engagement.state().unwrap();
    let run = state
        .active
        .validation_run_id
        .as_ref()
        .expect("persisted active validation");
    let validation = success(
        cli(&["validation", "status"], workspace)
            .arg("--run")
            .arg(run),
    );
    assert_eq!(validation["state"], "COMPLETE");
    assert_eq!(validation["rowsProcessed"], 4);
    assert_eq!(validation["issueCount"], 5);
    let profile = success(cli(&["profile", "inspect"], workspace).args([
        "--profile-id",
        PROFILE_ID,
        "--profile-version",
        "1.0.0",
    ]));
    assert_eq!(profile["authority"], "LOCAL_MAPPING_VALIDATION");
    assert_eq!(profile["approval"]["acknowledged"], true);
    assert_eq!(validation["resultHash"].as_str().unwrap().len(), 64);
    let status = success(&mut cli(&["status"], workspace));
    assert_eq!(status["complete_jobs"], 1);
    assert_eq!(status["jobs"][0]["stored_records"], 4);
    let integrity = success(&mut cli(&["integrity"], workspace));
    assert_eq!(integrity["ok"], true);
    assert_eq!(integrity["checked_profile_approvals"], 1);
    assert_eq!(integrity["checked_validation_runs"], 1);
    let database = Connection::open_with_flags(
        workspace.join("state.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let rows: Vec<(String, u64, String)> = database.prepare(
        "SELECT file_path, source_ordinal, fields_json FROM source_records ORDER BY file_path, source_ordinal"
    ).unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap()
        .collect::<rusqlite::Result<_>>().unwrap();
    assert_eq!(rows.len(), 4);
    let evidence = immutable_evidence(workspace);
    let acceptances: Vec<_> = evidence
        .iter()
        .filter(|(path, _)| path.starts_with("engagement/acceptances/"))
        .collect();
    assert_eq!(
        acceptances.len(),
        1,
        "one explicitly accepted synthetic source"
    );
    assert_eq!(acceptances[0].1["sha256"], validation["sourceHash"]);
    assert_eq!(acceptances[0].1["bytes"], status["jobs"][0]["source_size"]);
    json!({
        "workspace": authority.require_active().unwrap(),
        "authorityFileHash": sha256_hex(fs::read(workspace.join("workspace.json")).unwrap()),
        "engagement": engagement.detail().unwrap(), "status": status, "profile": profile,
        "validation": validation, "integrity": integrity, "sourceRowsHash": canonical_hash(&json!(rows)).unwrap(),
        "immutableEvidence": evidence,
    })
}

#[allow(clippy::too_many_lines)]
fn create_work(root: &Path, receipt: &Path) {
    let pending = create_project(root, "Synthetic resumable object").unwrap();
    let input = pending.input.join("valid.zip");
    fs::copy(repository_path("fixtures/packages/valid.zip"), &input).unwrap();
    assert!(!pending.workspace.root().join("state.sqlite3").exists());
    assert!(bind_project(&pending.descriptor_path).is_err());
    initialize_explicitly(pending.workspace.root());
    let project = bind_project(&pending.descriptor_path).unwrap();
    let engagement = service(project.workspace.root());
    engagement
        .create_or_reopen(&CreateOrReopenCommand {
            schema_version: CREATE_ENGAGEMENT_COMMAND_SCHEMA.to_owned(),
            display_name: "Synthetic customer lifecycle proof".to_owned(),
            idempotency_key: "proof-create-engagement-0001".to_owned(),
        })
        .unwrap();
    let reservation = engagement
        .reserve_preflight(&ReservePreflightCommand {
            schema_version: RESERVE_PREFLIGHT_COMMAND_SCHEMA.to_owned(),
            source_name: "valid.zip".to_owned(),
            idempotency_key: "proof-reserve-source-0001".to_owned(),
        })
        .unwrap();
    fs::copy(&input, &reservation.controlled_path).unwrap();
    let bytes = fs::read(&input).unwrap();
    let ready = engagement
        .complete_preflight(&CompletePreflightCommand {
            preflight_id: reservation.preflight.preflight_id,
            sha256: sha256_hex(&bytes),
            size_bytes: u64::try_from(bytes.len()).unwrap(),
        })
        .unwrap();
    assert_eq!(ready.state, SourcePreflightState::Ready);
    assert!(
        success(&mut cli(&["status"], project.workspace.root()))["jobs"]
            .as_array()
            .unwrap()
            .is_empty(),
        "placing and preflighting input must not accept it"
    );
    let mut acceptance = AcceptPreflightCommand {
        schema_version: ACCEPT_SOURCE_COMMAND_SCHEMA.to_owned(),
        preflight_id: ready.preflight_id,
        expected_source_hash: ready.source_hash.unwrap(),
        expected_manifest_hash: ready.manifest_hash.unwrap(),
        idempotency_key: "proof-accept-source-0001".to_owned(),
        acknowledgement: String::new(),
    };
    assert_eq!(
        engagement.accept_preflight(&acceptance).unwrap_err().code(),
        "POLICY_INVALID"
    );
    assert_eq!(
        success(&mut cli(&["status"], project.workspace.root()))["jobs"]
            .as_array()
            .unwrap()
            .as_slice(),
        &[] as &[Value]
    );
    SOURCE_ACCEPTANCE_ACKNOWLEDGEMENT.clone_into(&mut acceptance.acknowledgement);
    let accepted = engagement.accept_preflight(&acceptance).unwrap();
    assert_eq!(accepted.source_hash, sha256_hex(&bytes));
    let selected = engagement
        .select_object_pack(&SelectObjectPackCommand {
            schema_version: SELECT_OBJECT_PACK_COMMAND_SCHEMA.to_owned(),
            object_pack_id: PROFILE_ID.to_owned(),
            idempotency_key: "proof-select-pack-0001".to_owned(),
        })
        .unwrap();
    assert_eq!(
        selected.profile.lifecycle_status,
        ProfileLifecycleStatus::ReviewedDraft
    );
    assert_eq!(selected.profile.authority, ProfileAuthority::None);
    assert!(!selected.profile.can_start_new_runs);
    let approved = engagement
        .approve_profile(&ApproveProfileCommand {
            schema_version: APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA.to_owned(),
            actor: "synthetic-proof-operator".to_owned(),
            reason: "Explicitly approve exact reviewed synthetic fixture for local validation"
                .to_owned(),
            authority_acknowledgement: LOCAL_MAPPING_VALIDATION_AUTHORITY_ACKNOWLEDGEMENT
                .to_owned(),
            idempotency_key: "proof-approve-profile-0001".to_owned(),
        })
        .unwrap();
    assert_eq!(approved.profile_hash, selected.profile.profile_hash);
    assert_eq!(approved.authority, ProfileAuthority::LocalMappingValidation);
    assert_eq!(approved.lifecycle_status, ProfileLifecycleStatus::Approved);
    assert!(approved.can_start_new_runs);
    let prepared = engagement
        .prepare_mapping(&PrepareMappingCommand {
            schema_version: PREPARE_MAPPING_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "proof-prepare-mapping-0001".to_owned(),
            mapping_id: "synthetic.customer.lifecycle-proof".to_owned(),
            version: "1.0.0".to_owned(),
            selected_suggestion_ids: controlled_pack()
                .mapping_suggestions
                .into_iter()
                .map(|s| s.suggestion_id)
                .collect(),
            author: Some("synthetic-proof-operator".to_owned()),
            reviewer: Some("checked-in reviewed synthetic pack".to_owned()),
            acknowledgement: ASSISTED_MAPPING_ACKNOWLEDGEMENT.to_owned(),
        })
        .unwrap();
    let validation = engagement
        .launch_validation(&LaunchValidationCommand {
            schema_version: LAUNCH_VALIDATION_COMMAND_SCHEMA.to_owned(),
            idempotency_key: "proof-launch-validation-0001".to_owned(),
            expected_engagement_generation: engagement.state().unwrap().generation,
            expected_job_id: accepted.job_id,
            expected_mapping_id: prepared.document.mapping_id,
            expected_mapping_version: prepared.document.version,
            expected_mapping_hash: prepared.mapping_hash,
        })
        .unwrap();
    assert_eq!(validation.status.state, ValidationRunState::Complete);
    let snapshot = durable_snapshot(project.workspace.root());
    let report = json!({"writerPid": std::process::id(),
        "cliExecutable": env!("CARGO_BIN_EXE_migration-stream"),
        "testExecutable": std::env::current_exe().unwrap(), "descriptor": project.descriptor,
        "inputHash": sha256_hex(&bytes), "snapshot": snapshot});
    fs::write(receipt, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    // Returning from this child test drops every service/SQLite handle; the
    // parent waits for process exit before it launches the reopen driver.
}

fn assert_same_snapshot(actual: &Value, expected: &Value) {
    fn differences(actual: &Value, expected: &Value, path: &str, output: &mut Vec<String>) {
        if actual == expected {
            return;
        }
        match (actual, expected) {
            (Value::Object(a), Value::Object(b)) if a.keys().eq(b.keys()) => {
                for key in a.keys() {
                    differences(&a[key], &b[key], &format!("{path}/{key}"), output);
                }
            }
            (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
                for (i, (a, b)) in a.iter().zip(b).enumerate() {
                    differences(a, b, &format!("{path}/{i}"), output);
                }
            }
            _ => output.push(format!("{path}: actual={actual}, expected={expected}")),
        }
    }
    let mut changed = Vec::new();
    differences(actual, expected, "", &mut changed);
    assert!(
        changed.is_empty(),
        "fresh process snapshot differences: {changed:#?}"
    );
}

fn child(mode: &str, root: &Path, receipt: &Path) -> Output {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "project_lifecycle_child_driver",
            "--nocapture",
        ])
        .env("MIGRATION_STREAM_PROOF_MODE", mode)
        .env("MIGRATION_STREAM_PROOF_ROOT", root)
        .env("MIGRATION_STREAM_PROOF_RECEIPT", receipt)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{mode} child failed\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if let Some(directory) = std::env::var_os("MIGRATION_STREAM_PROOF_EVIDENCE_DIR") {
        let directory = PathBuf::from(directory);
        fs::create_dir_all(&directory).unwrap();
        let label = root.file_name().unwrap().to_str().unwrap();
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        fs::write(directory.join(format!("{label}-{mode}-{time}.json")), serde_json::to_vec_pretty(&json!({
            "stage": mode, "object": label, "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr), "success": output.status.success(),
        })).unwrap()).unwrap();
        if mode == "create-work" {
            fs::copy(receipt, directory.join(format!("{label}-snapshot.json"))).unwrap();
        }
    }
    output
}

#[test]
#[ignore = "test-only subprocess driver invoked by the lifecycle parent tests"]
fn project_lifecycle_child_driver() {
    let mode = std::env::var("MIGRATION_STREAM_PROOF_MODE").unwrap();
    let root = PathBuf::from(std::env::var_os("MIGRATION_STREAM_PROOF_ROOT").unwrap());
    let receipt = PathBuf::from(std::env::var_os("MIGRATION_STREAM_PROOF_RECEIPT").unwrap());
    let pending = root.join(PENDING_PROJECT_FILENAME);
    println!(
        "synthetic lifecycle stage={mode} pid={} root={}",
        std::process::id(),
        root.display()
    );
    match mode.as_str() {
        "create-work" => create_work(&root, &receipt),
        "verify-work" | "reject-mismatch" => {
            let expected: Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
            assert_ne!(
                expected["writerPid"].as_u64().unwrap(),
                u64::from(std::process::id())
            );
            if mode == "reject-mismatch" {
                assert!(open_project(root.join(PROJECT_FILENAME)).is_err());
            } else {
                let project = open_project(root.join(PROJECT_FILENAME)).unwrap();
                assert_eq!(
                    serde_json::to_value(project.descriptor).unwrap(),
                    expected["descriptor"]
                );
                assert_eq!(
                    sha256_hex(fs::read(project.input.join("valid.zip")).unwrap()),
                    expected["inputHash"]
                );
            }
            assert_same_snapshot(
                &durable_snapshot(&root.join("workspace")),
                &expected["snapshot"],
            );
        }
        "create-pending" => {
            create_project(&root, "Pending synthetic object").unwrap();
        }
        "inspect-pending" => {
            let project = open_project(pending).unwrap();
            assert!(project.descriptor.binding.is_none());
            assert!(!root.join(PROJECT_FILENAME).exists());
            assert!(matches!(
                project.lifecycle.state,
                ObjectWorkspaceState::InitializeRequired | ObjectWorkspaceState::Active
            ));
        }
        "initialize-pending" => {
            initialize_explicitly(&root.join("workspace"));
            assert!(open_project(pending).unwrap().descriptor.binding.is_none());
            assert!(!root.join(PROJECT_FILENAME).exists());
        }
        "bind-pending" => {
            bind_project(pending).unwrap();
        }
        other => panic!("unknown test child mode: {other}"),
    }
}

fn evidence_fixture(acceptance: Option<&[u8]>) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let mut files = vec![
        ("packages/package.zip", b"package".as_slice()),
        (
            "engagement/preflights/retained/source.zip",
            b"retained".as_slice(),
        ),
    ];
    if let Some(bytes) = acceptance {
        files.push(("engagement/acceptances/attempt.zip", bytes));
    }
    for (relative, bytes) in files {
        let path = root.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(path, permissions).unwrap();
    }
    root
}

#[test]
fn evidence_keys_are_portable_and_preserve_exact_acceptance_hashes() {
    let fixture = evidence_fixture(Some(b"accepted-source"));
    let evidence = immutable_evidence(fixture.path());
    assert_eq!(
        evidence.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "engagement/acceptances/attempt.zip",
            "engagement/preflights/retained/source.zip",
            "packages/package.zip",
        ]
    );
    assert_eq!(
        evidence["engagement/acceptances/attempt.zip"],
        json!({"sha256": sha256_hex(b"accepted-source"), "bytes": 15})
    );
}

#[test]
fn missing_or_empty_acceptance_evidence_cannot_silently_pass() {
    let fixture = evidence_fixture(None);
    assert!(std::panic::catch_unwind(|| immutable_evidence(fixture.path())).is_err());
    fs::create_dir(fixture.path().join("engagement/acceptances")).unwrap();
    assert!(std::panic::catch_unwind(|| immutable_evidence(fixture.path())).is_err());
}

#[test]
#[should_panic(expected = "fresh process snapshot differences")]
fn changed_acceptance_bytes_are_rejected_by_the_snapshot_comparison() {
    // These bounded fixtures exercise the hash walker/comparator only; real ZIP
    // intake and acceptance are exercised by the process lifecycle scenario.
    let original = evidence_fixture(Some(b"accepted-A"));
    let changed = evidence_fixture(Some(b"accepted-B"));
    let expected = immutable_evidence(original.path());
    let actual = immutable_evidence(changed.path());
    let acceptance = "engagement/acceptances/attempt.zip";
    assert_eq!(actual.len(), 3);
    assert_eq!(actual[acceptance]["bytes"], expected[acceptance]["bytes"]);
    assert_ne!(actual[acceptance]["sha256"], expected[acceptance]["sha256"]);
    for relative in [
        "packages/package.zip",
        "engagement/preflights/retained/source.zip",
    ] {
        assert_eq!(actual[relative], expected[relative]);
    }
    assert_same_snapshot(
        &json!({"immutableEvidence": actual}),
        &json!({"immutableEvidence": expected}),
    );
}

#[test]
fn two_objects_resume_exact_work_in_fresh_processes_and_keep_writer_isolation() {
    let temp = tempfile::tempdir().unwrap();
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    let receipt_a = temp.path().join("a-proof.json");
    let receipt_b = temp.path().join("b-proof.json");
    child("create-work", &a, &receipt_a);
    child("create-work", &b, &receipt_b);
    let expected_a: Value = serde_json::from_slice(&fs::read(&receipt_a).unwrap()).unwrap();
    let expected_b: Value = serde_json::from_slice(&fs::read(&receipt_b).unwrap()).unwrap();
    for pointer in [
        "/descriptor/projectId",
        "/snapshot/workspace/workspaceId",
        "/snapshot/engagement/state/engagementId",
        "/snapshot/validation/jobId",
        "/snapshot/validation/runId",
    ] {
        assert_ne!(
            expected_a.pointer(pointer).unwrap(),
            expected_b.pointer(pointer).unwrap(),
            "{pointer}"
        );
    }
    child("verify-work", &a, &receipt_a);
    child("verify-work", &b, &receipt_b);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(a.join("workspace/.migration-stream.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let rejected = cli(&["recover"], &a.join("workspace")).output().unwrap();
    assert!(!rejected.status.success());
    let error: Value = serde_json::from_slice(&rejected.stderr).unwrap();
    assert_eq!(error["code"], "WORKSPACE_LOCKED");
    success(&mut cli(&["recover"], &b.join("workspace")));
    drop(lock);
    success(&mut cli(&["recover"], &a.join("workspace")));
    let descriptor_path = a.join(PROJECT_FILENAME);
    let original = fs::read(&descriptor_path).unwrap();
    let mut altered = expected_a["descriptor"].clone();
    altered["binding"] = expected_b["descriptor"]["binding"].clone();
    fs::write(&descriptor_path, serde_json::to_vec(&altered).unwrap()).unwrap();
    child("reject-mismatch", &a, &receipt_a);
    fs::write(&descriptor_path, original).unwrap();
    child("verify-work", &a, &receipt_a);
    child("verify-work", &b, &receipt_b);
}

#[test]
fn pending_creation_survives_process_exit_without_implicit_establishment_or_binding() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("pending");
    let receipt = temp.path().join("unused.json");
    child("create-pending", &root, &receipt);
    child("inspect-pending", &root, &receipt);
    assert!(!root.join("workspace/state.sqlite3").exists());
    assert!(!root.join("workspace/workspace.json").exists());
    child("initialize-pending", &root, &receipt);
    let authority = fs::read(root.join("workspace/workspace.json")).unwrap();
    child("inspect-pending", &root, &receipt);
    assert!(!root.join(PROJECT_FILENAME).exists());
    child("bind-pending", &root, &receipt);
    let reopened = open_project(root.join(PROJECT_FILENAME)).unwrap();
    assert_eq!(reopened.lifecycle.state, ObjectWorkspaceState::Active);
    assert_eq!(
        authority,
        fs::read(root.join("workspace/workspace.json")).unwrap()
    );
    assert_eq!(
        success(&mut cli(&["status"], reopened.workspace.root()))["complete_jobs"],
        0
    );
}

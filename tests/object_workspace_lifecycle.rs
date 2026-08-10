use std::{
    fs,
    path::Path,
    sync::{Arc, Barrier},
    thread,
};

use migration_stream::{
    KernelError,
    object_workspace::{
        ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA, EstablishObjectWorkspaceCommand,
        INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA, OBJECT_WORKSPACE_DESCRIPTOR_FILENAME,
        ObjectWorkspaceService, ObjectWorkspaceState,
    },
    state::{integrity_check, recover_workspace},
};
use rusqlite::Connection;
use serde_json::Value;

const DISPLAY_NAME: &str = "Sales document readiness";
const OBJECT_SCOPE: &str = "Synthetic sales document";
const CREATOR: &str = "Lifecycle integration operator";

fn initialize_command(
    service: &ObjectWorkspaceService,
    idempotency_key: &str,
) -> EstablishObjectWorkspaceCommand {
    let status = service.inspect().expect("inspect uninitialized workspace");
    assert_eq!(status.state, ObjectWorkspaceState::InitializeRequired);
    EstablishObjectWorkspaceCommand {
        schema_version: INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA.to_owned(),
        expected_state_hash: status.state_hash,
        display_name: DISPLAY_NAME.to_owned(),
        migration_object_name_or_scope: OBJECT_SCOPE.to_owned(),
        creator_attribution: CREATOR.to_owned(),
        idempotency_key: idempotency_key.to_owned(),
    }
}

fn initialize(
    root: &Path,
) -> (
    EstablishObjectWorkspaceCommand,
    migration_stream::object_workspace::ObjectWorkspaceRecord,
) {
    let service = ObjectWorkspaceService::new(root).expect("workspace service");
    let command = initialize_command(&service, "workspace-lifecycle-init-0001");
    let record = service.initialize(&command).expect("initialize workspace");
    (command, record)
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

fn rewrite_descriptor(root: &Path, bytes: &[u8]) {
    let path = root.join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
    make_owner_writable(&path);
    fs::write(&path, bytes).expect("replace descriptor bytes");
    let mut permissions = fs::metadata(&path)
        .expect("rewritten metadata")
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).expect("restore descriptor read-only");
}

fn assert_integrity_failure(root: &Path) {
    let report = integrity_check(root).expect("global integrity report");
    assert!(!report.ok);
    assert!(report.issues.iter().any(|issue| {
        issue.contains("object workspace descriptor")
            || issue.contains("object workspace authority")
    }));
}

#[test]
fn fresh_initialize_exact_retry_conflict_and_stale_state_fail_closed() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    let service = ObjectWorkspaceService::new(temporary.path()).expect("workspace service");
    let initial = service.inspect().expect("initial state");
    assert_eq!(initial.state, ObjectWorkspaceState::InitializeRequired);
    assert_eq!(
        initial.required_action,
        Some(migration_stream::object_workspace::ObjectWorkspaceAction::Initialize)
    );

    let command = initialize_command(&service, "workspace-lifecycle-init-0001");
    let first = service.initialize(&command).expect("first initialize");
    let retry = service.initialize(&command).expect("exact retry");
    assert_eq!(retry, first);

    let mut conflicting_retry = command.clone();
    conflicting_retry.display_name = "Conflicting workspace identity".to_owned();
    assert!(matches!(
        service.initialize(&conflicting_retry),
        Err(KernelError::Conflict(_))
    ));

    let stale_command = EstablishObjectWorkspaceCommand {
        idempotency_key: "workspace-lifecycle-stale-0002".to_owned(),
        ..command
    };
    assert!(matches!(
        service.initialize(&stale_command),
        Err(KernelError::Conflict(_))
    ));
    assert_eq!(service.require_active().expect("active identity"), first);
}

#[test]
fn concurrent_competing_initialization_commits_one_complete_identity() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    let root = temporary.path().to_path_buf();
    let service = ObjectWorkspaceService::new(&root).expect("workspace service");
    let first_command = initialize_command(&service, "workspace-concurrent-first-0001");
    let mut second_command = first_command.clone();
    second_command.display_name = "Competing workspace identity".to_owned();
    second_command.idempotency_key = "workspace-concurrent-second-0002".to_owned();

    let barrier = Arc::new(Barrier::new(3));
    let first_root = root.clone();
    let first_barrier = Arc::clone(&barrier);
    let first = thread::spawn(move || {
        let service = ObjectWorkspaceService::new(first_root).expect("first service");
        first_barrier.wait();
        service.initialize(&first_command)
    });
    let second_root = root.clone();
    let second_barrier = Arc::clone(&barrier);
    let second = thread::spawn(move || {
        let service = ObjectWorkspaceService::new(second_root).expect("second service");
        second_barrier.wait();
        service.initialize(&second_command)
    });
    barrier.wait();

    let results = [
        first.join().expect("first initializer thread"),
        second.join().expect("second initializer thread"),
    ];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert!(
        results
            .iter()
            .filter(|result| result.is_err())
            .all(|result| {
                matches!(
                    result,
                    Err(KernelError::Conflict(_) | KernelError::WorkspaceLocked(_))
                )
            })
    );

    let reopened = ObjectWorkspaceService::new(&root)
        .expect("reopened service")
        .require_active()
        .expect("one complete active identity");
    assert!(
        reopened.display_name == DISPLAY_NAME
            || reopened.display_name == "Competing workspace identity"
    );
    let report = integrity_check(&root).expect("post-race integrity");
    assert!(report.ok, "{:?}", report.issues);
    assert_eq!(report.checked_object_workspaces, 1);
}

#[test]
fn reload_and_reopen_preserve_the_exact_workspace_identity() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    let (_, initialized) = initialize(temporary.path());
    let descriptor_before = fs::read(temporary.path().join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME))
        .expect("descriptor before reopen");

    let reloaded = ObjectWorkspaceService::new(temporary.path())
        .expect("reloaded service")
        .inspect()
        .expect("reloaded status");
    assert_eq!(reloaded.state, ObjectWorkspaceState::Active);
    assert_eq!(reloaded.workspace.as_ref(), Some(&initialized));

    drop(reloaded);
    let reopened = ObjectWorkspaceService::new(temporary.path())
        .expect("process-style reopen")
        .require_active()
        .expect("reopened identity");
    assert_eq!(reopened, initialized);
    assert_eq!(
        descriptor_before,
        fs::read(temporary.path().join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME))
            .expect("descriptor after reopen")
    );
}

#[test]
fn corrupt_descriptor_fails_closed_and_global_replay_reports_it() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize(temporary.path());
    rewrite_descriptor(temporary.path(), b"{not-json");

    let error = ObjectWorkspaceService::new(temporary.path())
        .expect("workspace service")
        .inspect()
        .expect_err("corrupt descriptor must fail");
    assert!(matches!(error, KernelError::IntegrityFailed(_)));
    assert!(error.to_string().contains("workspace.json is invalid"));
    assert_integrity_failure(temporary.path());
}

#[test]
fn oversized_descriptor_fails_closed_and_global_replay_reports_it() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize(temporary.path());
    rewrite_descriptor(temporary.path(), &vec![b'x'; 16 * 1024 + 1]);

    let error = ObjectWorkspaceService::new(temporary.path())
        .expect("workspace service")
        .inspect()
        .expect_err("oversized descriptor must fail");
    assert!(matches!(error, KernelError::IntegrityFailed(_)));
    assert!(error.to_string().contains("1..=16384 bytes"));
    assert_integrity_failure(temporary.path());
}

#[test]
fn unsupported_descriptor_version_fails_closed() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize(temporary.path());
    let path = temporary.path().join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
    let mut descriptor: Value = serde_json::from_slice(&fs::read(&path).expect("descriptor bytes"))
        .expect("descriptor JSON");
    descriptor["schemaVersion"] =
        Value::String("migration-stream/object-workspace/v999".to_owned());
    rewrite_descriptor(
        temporary.path(),
        &serde_json::to_vec(&descriptor).expect("canonical descriptor JSON"),
    );

    let error = ObjectWorkspaceService::new(temporary.path())
        .expect("workspace service")
        .inspect()
        .expect_err("unsupported descriptor version must fail");
    assert!(matches!(error, KernelError::IntegrityFailed(_)));
    assert!(error.to_string().contains("schema version is unsupported"));
    assert_integrity_failure(temporary.path());
}

#[cfg(unix)]
#[test]
fn descriptor_symlink_fails_closed_and_global_replay_reports_it() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize(temporary.path());
    let descriptor = temporary.path().join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
    let target = temporary.path().join("descriptor-target.json");
    fs::copy(&descriptor, &target).expect("copy descriptor target");
    fs::remove_file(&descriptor).expect("remove original descriptor");
    symlink(&target, &descriptor).expect("symlink descriptor");

    let error = ObjectWorkspaceService::new(temporary.path())
        .expect("workspace service")
        .inspect()
        .expect_err("symlink descriptor must fail");
    assert!(matches!(error, KernelError::IntegrityFailed(_)));
    assert!(error.to_string().contains("real regular non-link file"));
    assert_integrity_failure(temporary.path());
}

#[test]
fn nonregular_descriptor_fails_closed_without_creating_database_authority() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    fs::create_dir(temporary.path().join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME))
        .expect("create nonregular descriptor entry");

    let service = ObjectWorkspaceService::new(temporary.path()).expect("workspace service");
    assert!(matches!(
        service.inspect(),
        Err(KernelError::IntegrityFailed(_))
    ));
    assert!(
        !temporary.path().join("state.sqlite3").exists(),
        "descriptor inspection must not create SQLite authority"
    );
    assert_integrity_failure(temporary.path());
}

#[test]
fn descriptor_and_database_mismatch_fail_closed() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize(temporary.path());

    let connection =
        Connection::open(temporary.path().join("state.sqlite3")).expect("workspace database");
    connection
        .execute_batch(
            "DROP TRIGGER object_workspace_bindings_no_update;
             UPDATE object_workspace_bindings
             SET display_name = 'Database-only identity'
             WHERE singleton = 1;
             CREATE TRIGGER object_workspace_bindings_no_update
                 BEFORE UPDATE ON object_workspace_bindings
                 BEGIN
                     SELECT RAISE(ABORT, 'object workspace binding is immutable');
                 END;",
        )
        .expect("simulate database identity tamper");
    drop(connection);

    let error = ObjectWorkspaceService::new(temporary.path())
        .expect("workspace service")
        .inspect()
        .expect_err("descriptor/database mismatch must fail");
    assert!(matches!(error, KernelError::IntegrityFailed(_)));
    assert!(
        error
            .to_string()
            .contains("workspace.json and SQLite object workspace authority do not agree")
    );
    assert_integrity_failure(temporary.path());
}

#[test]
fn missing_descriptor_with_bound_database_fails_global_integrity_replay() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize(temporary.path());
    fs::remove_file(temporary.path().join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME))
        .expect("remove bound descriptor");

    let service = ObjectWorkspaceService::new(temporary.path()).expect("workspace service");
    assert!(matches!(
        service.inspect(),
        Err(KernelError::IntegrityFailed(_))
    ));
    assert_integrity_failure(temporary.path());
}

#[test]
fn active_newer_database_schema_locks_workspace_and_global_replay() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize(temporary.path());
    let connection =
        Connection::open(temporary.path().join("state.sqlite3")).expect("workspace database");
    connection
        .pragma_update(None, "user_version", 999_i64)
        .expect("simulate unsupported future schema");
    drop(connection);

    let service = ObjectWorkspaceService::new(temporary.path()).expect("workspace service");
    let error = service
        .inspect()
        .expect_err("unsupported active schema must fail closed");
    assert!(matches!(error, KernelError::IntegrityFailed(_)));
    assert!(error.to_string().contains("newer than supported"));
    assert!(matches!(
        service.require_active(),
        Err(KernelError::IntegrityFailed(_))
    ));
    assert_integrity_failure(temporary.path());
}

#[test]
fn missing_or_weakened_workspace_immutability_guards_fail_replay() {
    let guards = [
        (
            "object_workspace_bindings",
            "object_workspace_bindings_no_update",
            "UPDATE",
        ),
        (
            "object_workspace_bindings",
            "object_workspace_bindings_no_delete",
            "DELETE",
        ),
        (
            "object_workspace_engagement_bindings",
            "object_workspace_engagement_bindings_no_update",
            "UPDATE",
        ),
        (
            "object_workspace_engagement_bindings",
            "object_workspace_engagement_bindings_no_delete",
            "DELETE",
        ),
    ];
    for (table, trigger, operation) in guards {
        let missing = tempfile::tempdir().expect("missing-guard workspace");
        initialize(missing.path());
        let connection =
            Connection::open(missing.path().join("state.sqlite3")).expect("workspace database");
        connection
            .execute_batch(&format!("DROP TRIGGER {trigger};"))
            .expect("drop immutable guard");
        drop(connection);
        let service = ObjectWorkspaceService::new(missing.path()).expect("workspace service");
        let error = service.inspect().expect_err("missing guard must fail");
        assert!(error.to_string().contains(trigger));
        assert_integrity_failure(missing.path());

        let weakened = tempfile::tempdir().expect("weakened-guard workspace");
        initialize(weakened.path());
        let connection =
            Connection::open(weakened.path().join("state.sqlite3")).expect("workspace database");
        connection
            .execute_batch(&format!(
                "DROP TRIGGER {trigger};
                 CREATE TRIGGER {trigger}
                 BEFORE {operation} ON {table}
                 BEGIN
                     SELECT 1;
                 END;"
            ))
            .expect("replace immutable guard with no-op");
        drop(connection);
        let service = ObjectWorkspaceService::new(weakened.path()).expect("workspace service");
        let error = service.inspect().expect_err("weakened guard must fail");
        assert!(error.to_string().contains(trigger));
        assert_integrity_failure(weakened.path());
    }
}

#[test]
fn failed_binding_transaction_removes_new_descriptor_and_leaves_no_authority_row() {
    let temporary = tempfile::tempdir().expect("temporary workspace");
    initialize(temporary.path());
    let descriptor = temporary.path().join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
    make_owner_writable(&descriptor);
    fs::remove_file(&descriptor).expect("strip initialized descriptor");
    let database = Connection::open(temporary.path().join("state.sqlite3"))
        .expect("open schema-only database");
    database
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TRIGGER object_workspace_engagement_bindings_no_update;
             DROP TRIGGER object_workspace_engagement_bindings_no_delete;
             DROP TABLE object_workspace_engagement_bindings;
             DROP TRIGGER object_workspace_bindings_no_update;
             DROP TRIGGER object_workspace_bindings_no_delete;
             DROP TABLE object_workspace_bindings;
             CREATE TABLE object_workspace_engagement_bindings (
                 incompatible_column TEXT NOT NULL
             );",
        )
        .expect("create deterministic transaction collision");
    drop(database);

    let service = ObjectWorkspaceService::new(temporary.path()).expect("workspace service");
    let status = service
        .inspect()
        .expect("inspect adoption-required workspace");
    assert_eq!(status.state, ObjectWorkspaceState::AdoptRequired);
    let command = EstablishObjectWorkspaceCommand {
        schema_version: ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA.to_owned(),
        expected_state_hash: status.state_hash,
        display_name: DISPLAY_NAME.to_owned(),
        migration_object_name_or_scope: OBJECT_SCOPE.to_owned(),
        creator_attribution: CREATOR.to_owned(),
        idempotency_key: "workspace-rollback-adopt-0001".to_owned(),
    };
    assert!(
        service.adopt(&command).is_err(),
        "incompatible durable schema must make establishment fail"
    );
    assert!(
        !temporary
            .path()
            .join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME)
            .exists(),
        "a returned pre-commit failure must not leave a published descriptor"
    );
    let database =
        Connection::open(temporary.path().join("state.sqlite3")).expect("reopen database");
    let binding_table: bool = database
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_schema
                 WHERE type = 'table' AND name = 'object_workspace_bindings'
             )",
            [],
            |row| row.get(0),
        )
        .expect("inspect rolled-back authority table");
    assert!(!binding_table, "binding DDL must roll back with the row");
}

#[cfg(unix)]
#[test]
fn live_service_rejects_same_path_root_replacement() {
    let parent = tempfile::tempdir().expect("temporary parent");
    let selected = parent.path().join("selected-workspace");
    let displaced = parent.path().join("displaced-workspace");
    fs::create_dir(&selected).expect("create startup-selected directory");
    let service = ObjectWorkspaceService::new(&selected).expect("workspace service");
    let command = initialize_command(&service, "workspace-root-identity-0001");
    service
        .initialize(&command)
        .expect("initialize selected root");

    fs::rename(&selected, &displaced).expect("displace selected root");
    fs::create_dir(&selected).expect("replace selected path with a real directory");
    let error = service
        .inspect()
        .expect_err("retained service must reject same-path root replacement");
    assert!(matches!(error, KernelError::IntegrityFailed(_)));
    assert!(error.to_string().contains("changed identity"));
}

#[test]
fn ordinary_kernel_mutation_requires_active_workspace_authority() {
    let empty = tempfile::tempdir().expect("empty selected workspace");
    let entries_before = fs::read_dir(empty.path()).expect("empty entries").count();
    assert!(matches!(
        recover_workspace(empty.path()),
        Err(KernelError::Conflict(_))
    ));
    assert_eq!(
        fs::read_dir(empty.path())
            .expect("entries after rejected recovery")
            .count(),
        entries_before,
        "rejected fresh mutation must create no database or lock"
    );

    let legacy = tempfile::tempdir().expect("legacy selected workspace");
    initialize(legacy.path());
    let descriptor = legacy.path().join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
    let database_path = legacy.path().join("state.sqlite3");
    let connection = Connection::open(&database_path).expect("bound database");
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TRIGGER object_workspace_engagement_bindings_no_update;
             DROP TRIGGER object_workspace_engagement_bindings_no_delete;
             DROP TABLE object_workspace_engagement_bindings;
             DROP TRIGGER object_workspace_bindings_no_update;
             DROP TRIGGER object_workspace_bindings_no_delete;
             DROP TABLE object_workspace_bindings;
             INSERT INTO jobs (
                 id, state, source_name, source_path, policy_json,
                 created_at, updated_at
             ) VALUES (
                 'legacy-job', 'SPOOLING', 'synthetic.zip',
                 'legacy/source.zip', '{}',
                 '2026-07-29T00:00:00.000Z',
                 '2026-07-29T00:00:00.000Z'
             );",
        )
        .expect("construct copied pre-descriptor evidence");
    drop(connection);
    make_owner_writable(&descriptor);
    fs::remove_file(&descriptor).expect("strip descriptor from copied legacy workspace");
    let database_before = fs::read(&database_path).expect("legacy database bytes");

    assert!(matches!(
        recover_workspace(legacy.path()),
        Err(KernelError::Conflict(_))
    ));
    assert_eq!(
        fs::read(&database_path).expect("legacy database after rejected recovery"),
        database_before,
        "adoption-required mutation must preserve existing database bytes"
    );
    assert_eq!(
        ObjectWorkspaceService::new(legacy.path())
            .expect("legacy service")
            .inspect()
            .expect("legacy state")
            .state,
        ObjectWorkspaceState::AdoptRequired
    );
}

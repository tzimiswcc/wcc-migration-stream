//! Real trusted project CLI operations; no runtime server is started here.

use migration_stream::canonical::sha256_hex;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    process::Command,
};

fn cli(arguments: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_migration-stream"));
    command.args(arguments);
    command
}

fn response(name: &str, command: &mut Command, expected: bool) -> Value {
    let output = command.output().unwrap();
    assert_eq!(
        output.status.success(),
        expected,
        "{name}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(if expected {
        &output.stdout
    } else {
        &output.stderr
    })
    .unwrap();
    assert_eq!(value["ok"], expected);
    if let Some(directory) = std::env::var_os("MIGRATION_STREAM_PROJECT_CLI_EVIDENCE_DIR") {
        let directory = PathBuf::from(directory);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
    if expected {
        value["result"].clone()
    } else {
        value
    }
}

fn create(root: &Path, initialized: bool) -> Command {
    let mut command = cli(&["project", "create"]);
    command
        .arg("--directory")
        .arg(root)
        .args(["--display-name", "Synthetic project CLI"]);
    if initialized {
        command.args([
            "--initialize-and-bind",
            "--migration-object-name-or-scope",
            "Synthetic local CLI object",
            "--creator-attribution",
            "Project CLI proof operator",
        ]);
    }
    command
}

fn inspect(project: &Path) -> Command {
    let mut command = cli(&["project", "inspect"]);
    command.arg("--project").arg(project);
    command
}

fn bind(project: &Path) -> Command {
    let mut command = cli(&["project", "bind"]);
    command.arg("--project").arg(project);
    command
}

fn initialize(name: &str, workspace: &Path) -> Value {
    response(
        name,
        cli(&["object-workspace", "initialize"])
            .arg("--workspace")
            .arg(workspace)
            .args([
                "--display-name",
                "Synthetic project CLI",
                "--migration-object-name-or-scope",
                "Synthetic local CLI object",
                "--creator-attribution",
                "Project CLI proof operator",
            ]),
        true,
    )
}

fn path(value: &Value, key: &str) -> PathBuf {
    PathBuf::from(value[key].as_str().unwrap())
}

// The synthetic workspace lock has no payload. Windows byte-range locks
// prohibit even an empty ReadFile, so snapshot this exact control entry by
// no-follow metadata and native identity; all other file bytes remain hashed.
fn lock_sentinel_snapshot(path: &Path) -> String {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.file_type().is_file());
    assert_eq!(metadata.len(), 0, "synthetic lock sentinel must be empty");
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt as _;
        format!(
            "{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.mode(),
            metadata.nlink()
        )
    };
    #[cfg(windows)]
    let identity = {
        use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        assert_eq!(metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT, 0);
        let file = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
            .unwrap();
        let opened = file.metadata().unwrap();
        assert!(opened.file_type().is_file());
        assert_eq!(opened.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT, 0);
        assert_eq!(opened.len(), 0);
        format!(
            "{:?}",
            migration_stream_windows_native::file_identity_from_open_file(&file).unwrap()
        )
    };
    format!(
        "CONTROL_SENTINEL:{identity}:readonly={}:modified={:?}",
        metadata.permissions().readonly(),
        metadata.modified().unwrap()
    )
}

fn tree(root: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(root: &Path, directory: &Path, output: &mut BTreeMap<PathBuf, String>) {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                walk(root, &path, output);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                let snapshot = if relative == Path::new("workspace/.migration-stream.lock") {
                    lock_sentinel_snapshot(&path)
                } else {
                    sha256_hex(fs::read(&path).unwrap())
                };
                output.insert(relative.to_path_buf(), snapshot);
            }
        }
    }
    let mut result = BTreeMap::new();
    walk(root, root, &mut result);
    result
}

#[test]
fn tree_snapshot_retains_exact_sentinel_identity_and_other_file_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("object");
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let sentinel = workspace.join(".migration-stream.lock");
    fs::write(&sentinel, []).unwrap();
    let ordinary = root.join(".migration-stream.lock");
    fs::write(&ordinary, b"business bytes").unwrap();
    let before = tree(&root);
    assert_eq!(before.len(), 2);
    fs::write(&ordinary, b"changed bytes").unwrap();
    let changed = tree(&root);
    assert_ne!(
        changed[Path::new(".migration-stream.lock")],
        before[Path::new(".migration-stream.lock")]
    );
    assert_eq!(
        changed[Path::new("workspace/.migration-stream.lock")],
        before[Path::new("workspace/.migration-stream.lock")]
    );
    fs::rename(&sentinel, temp.path().join("retained-old-sentinel")).unwrap();
    assert!(!tree(&root).contains_key(Path::new("workspace/.migration-stream.lock")));
    fs::write(&sentinel, []).unwrap();
    assert_ne!(
        tree(&root)[Path::new("workspace/.migration-stream.lock")],
        before[Path::new("workspace/.migration-stream.lock")]
    );
    fs::write(&sentinel, b"unexpected payload").unwrap();
    assert!(std::panic::catch_unwind(|| tree(&root)).is_err());
}

fn assert_no_intake_or_profile_authority(workspace: &Path) {
    let database = Connection::open_with_flags(
        workspace.join("state.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    for table in [
        "jobs",
        "source_records",
        "metadata_snapshots",
        "profile_versions",
        "profile_approvals",
    ] {
        let count: i64 = database
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            count, 0,
            "{table} must remain untouched by project commands"
        );
    }
}

#[test]
fn pending_create_inspect_and_binding_keep_workspace_establishment_explicit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("pending");
    let pending = response("pending-create", &mut create(&root, false), true);
    assert_eq!(pending["schemaVersion"], "migration-stream/project-cli/v1");
    assert!(pending["descriptor"]["binding"].is_null());
    assert_eq!(pending["requiredAction"], "INITIALIZE_WORKSPACE_EXPLICITLY");
    let descriptor = path(&pending, "descriptorPath");
    let workspace = path(&pending, "workspacePath");
    assert!(!workspace.join("state.sqlite3").exists());
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/packages/valid.zip");
    fs::copy(fixture, path(&pending, "inputPath").join("unaccepted.zip")).unwrap();
    let before = tree(&root);
    let inspected = response("pending-inspect", &mut inspect(&descriptor), true);
    assert_eq!(pending, inspected);
    assert_eq!(tree(&root), before);
    let not_active = response("bind-before-initialize", &mut bind(&descriptor), false);
    assert_eq!(not_active["code"], "CONFLICT");
    assert!(!workspace.join("state.sqlite3").exists());
    let established = initialize("pending-workspace-initialize", &workspace);
    let authority = fs::read(workspace.join("workspace.json")).unwrap();
    let ready = response("active-pending-inspect", &mut inspect(&descriptor), true);
    assert_eq!(ready["requiredAction"], "BIND_PROJECT");
    assert!(ready["descriptor"]["binding"].is_null());
    let bound = response("explicit-project-bind", &mut bind(&descriptor), true);
    assert!(bound["requiredAction"].is_null());
    assert_eq!(
        bound["descriptor"]["binding"]["workspaceId"],
        established["workspaceId"]
    );
    assert_eq!(
        fs::read(workspace.join("workspace.json")).unwrap(),
        authority
    );
    assert_no_intake_or_profile_authority(&workspace);
}

#[test]
fn explicit_new_empty_initbind_is_one_operation_and_never_overwrites_or_resumes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("new-object");
    let bound = response("new-bound-create", &mut create(&root, true), true);
    assert_eq!(bound["lifecycle"]["state"], "ACTIVE");
    assert_eq!(
        bound["lifecycle"]["workspace"]["establishedBy"],
        "INITIALIZED"
    );
    assert!(bound["requiredAction"].is_null());
    let workspace = path(&bound, "workspacePath");
    let descriptor = path(&bound, "descriptorPath");
    let profile = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/wp003/synthetic-profile.reviewed.json");
    fs::copy(
        profile,
        path(&bound, "inputPath").join("unapproved-profile.json"),
    )
    .unwrap();
    let before = tree(&root);
    assert_eq!(
        response("bound-inspect", &mut inspect(&descriptor), true),
        bound
    );
    assert_eq!(tree(&root), before);
    let collision = response("bound-create-collision", &mut create(&root, true), false);
    assert_eq!(collision["code"], "IO");
    assert_eq!(collision["project"]["phase"], "CREATE");
    assert_eq!(tree(&root), before);
    assert_no_intake_or_profile_authority(&workspace);
    let existing = temp.path().join("existing");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("sentinel"), b"preserve").unwrap();
    response(
        "existing-folder-collision",
        &mut create(&existing, true),
        false,
    );
    assert_eq!(fs::read(existing.join("sentinel")).unwrap(), b"preserve");
    assert!(!existing.join("workspace").exists());
}

#[test]
fn explicit_metadata_and_partial_initialization_failure_are_actionable_without_autoresume() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing-metadata");
    let output = create(&missing, false)
        .arg("--initialize-and-bind")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!missing.exists());
    let undesired = temp.path().join("metadata-without-intent");
    let output = create(&undesired, false)
        .args(["--creator-attribution", "Operator"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!undesired.exists());
    let root = temp.path().join("partial");
    let failure = response(
        "partial-initialize-error",
        create(&root, false).args([
            "--initialize-and-bind",
            "--migration-object-name-or-scope",
            "Synthetic",
            "--creator-attribution",
            "",
        ]),
        false,
    );
    assert_eq!(failure["code"], "POLICY_INVALID");
    assert_eq!(failure["project"]["phase"], "INITIALIZE");
    assert!(
        failure["project"]["nextAction"]
            .as_str()
            .unwrap()
            .contains("explicit")
    );
    let pending = path(&failure["project"], "descriptorPath");
    assert!(pending.is_file());
    let before = tree(&root);
    response(
        "partial-create-retry-rejected",
        &mut create(&root, true),
        false,
    );
    assert_eq!(tree(&root), before);
    let view = response("partial-inspect", &mut inspect(&pending), true);
    assert_eq!(view["requiredAction"], "INITIALIZE_WORKSPACE_EXPLICITLY");
    assert!(!path(&view, "workspacePath").join("state.sqlite3").exists());
}

#[test]
fn invalid_version_escaping_reference_and_mismatched_identity_fail_without_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let a = response(
        "invalid-a-create",
        &mut create(&temp.path().join("a"), true),
        true,
    );
    let b = response(
        "invalid-b-create",
        &mut create(&temp.path().join("b"), true),
        true,
    );
    let descriptor = path(&a, "descriptorPath");
    let original = fs::read(&descriptor).unwrap();
    let root = descriptor.parent().unwrap();
    for (label, field, value) in [
        (
            "bad-version",
            "schemaVersion",
            Value::from("unsupported-project-version"),
        ),
        (
            "escaping-workspace",
            "workspace",
            Value::from("../b/workspace"),
        ),
        (
            "mismatched-workspace",
            "binding",
            b["descriptor"]["binding"].clone(),
        ),
    ] {
        let mut changed = a["descriptor"].clone();
        changed[field] = value;
        fs::write(&descriptor, serde_json::to_vec(&changed).unwrap()).unwrap();
        let before = tree(root);
        let failure = response(label, &mut inspect(&descriptor), false);
        assert_eq!(failure["code"], "INTEGRITY_FAILED");
        assert_eq!(tree(root), before);
        fs::write(&descriptor, &original).unwrap();
    }
    assert_eq!(
        response("restored-inspect", &mut inspect(&descriptor), true),
        a
    );
    assert_no_intake_or_profile_authority(&path(&a, "workspacePath"));
}

#[test]
fn explicit_bind_obeys_existing_writer_lock_while_inspect_remains_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("writer-held");
    let pending = response("writer-pending-create", &mut create(&root, false), true);
    let descriptor = path(&pending, "descriptorPath");
    let workspace = path(&pending, "workspacePath");
    initialize("writer-workspace-initialize", &workspace);
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(workspace.join(".migration-stream.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let before = tree(&root);
    let active = response(
        "writer-held-readonly-inspect",
        &mut inspect(&descriptor),
        true,
    );
    assert_eq!(active["requiredAction"], "BIND_PROJECT");
    assert_eq!(tree(&root), before);
    let rejected = response("writer-held-bind-rejected", &mut bind(&descriptor), false);
    assert_eq!(rejected["code"], "WORKSPACE_LOCKED");
    assert_eq!(rejected["project"]["phase"], "BIND");
    assert_eq!(tree(&root), before);
    drop(lock);
    let bound = response("writer-released-bind", &mut bind(&descriptor), true);
    assert_eq!(
        bound["descriptor"]["binding"]["workspaceId"],
        active["lifecycle"]["workspace"]["workspaceId"]
    );
    assert_no_intake_or_profile_authority(&workspace);
}

#[cfg(unix)]
#[test]
fn unrepresentable_path_errors_stay_json_without_overwrite_or_panic() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt as _};
    let temp = tempfile::tempdir().unwrap();
    let root = temp
        .path()
        .join(OsStr::from_bytes(b"synthetic-invalid-\xff"));
    let failure = response(
        "non-utf8-path-controlled-error",
        &mut create(&root, false),
        false,
    );
    assert!(matches!(failure["code"].as_str(), Some("IO" | "JSON")));
    assert_eq!(failure["project"]["phase"], "CREATE");
    assert_eq!(failure["project"]["pathsAreLossy"], true);
    // Filesystem support differs. A retained partial is preserved if creation
    // reached the response boundary; unsupported names fail without creation.
    if root.exists() {
        assert!(root.join("project.pending.mstream").exists());
        assert!(!root.join("workspace/state.sqlite3").exists());
        let before = tree(&root);
        response(
            "non-utf8-create-retry-rejected",
            &mut create(&root, false),
            false,
        );
        assert_eq!(tree(&root), before);
    }
}

use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

const VALID_PACKAGE_SHA256: &str =
    "656ed753f1dd7973906f428f526d9069c00c60c84ddc490599328d2217746ca3";
const EDMX_SNAPSHOT_SHA256: &str =
    "393b3740cc8dc6195633dd52b44f5a2d5a5dab30f12ce1ed0a66aaf5500118aa";
const CATALOGUE_PAYLOAD_SHA256: &str =
    "0e4089ab45a4b8316c2386d4449c6fa2befbc5fce2d257ce931e8035d6f40292";

mod support {
    use super::*;

    pub fn project_path(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
    }

    pub fn cli() -> Command {
        Command::new(env!("CARGO_BIN_EXE_migration-stream"))
    }

    pub fn initialize_workspace(path: &Path) {
        fs::create_dir_all(path).expect("create test object workspace");
        migration_stream::object_workspace::establish_for_local_automation(
            path,
            "CLI test workspace",
            "Synthetic migration object",
            "Migration Stream CLI test suite",
        )
        .expect("initialize CLI test object workspace");
    }

    pub fn output(command: &mut Command) -> Output {
        command
            .output()
            .expect("run migration-stream child process")
    }

    pub fn success(command: &mut Command) -> Value {
        let output = output(command);
        assert!(
            output.status.success(),
            "CLI failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let document: Value = serde_json::from_slice(&output.stdout).expect("success JSON");
        assert_eq!(document.get("ok"), Some(&Value::Bool(true)));
        document
            .get("result")
            .cloned()
            .expect("success response result")
    }

    pub fn failure(command: &mut Command) -> Value {
        let output = output(command);
        assert!(
            !output.status.success(),
            "CLI unexpectedly succeeded: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let document: Value = serde_json::from_slice(&output.stderr).expect("failure JSON");
        assert_eq!(document.get("ok"), Some(&Value::Bool(false)));
        document
    }

    pub fn path_field(value: &Value, key: &str) -> PathBuf {
        PathBuf::from(value[key].as_str().expect("JSON path field"))
    }

    pub fn assert_read_only(path: &Path) {
        let permissions = fs::metadata(path)
            .unwrap_or_else(|error| panic!("metadata for {}: {error}", path.display()))
            .permissions();
        assert!(permissions.readonly(), "{} is writable", path.display());
    }

    pub fn make_writable(path: &Path) {
        let mut permissions = fs::metadata(path)
            .unwrap_or_else(|error| panic!("metadata for {}: {error}", path.display()))
            .permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(permissions.mode() | 0o200);
        }
        #[cfg(not(unix))]
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "this non-Unix branch clears only the platform read-only attribute"
        )]
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)
            .unwrap_or_else(|error| panic!("make {} writable: {error}", path.display()));
    }

    pub fn sha256(path: &Path) -> String {
        let mut input =
            File::open(path).unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let count = input.read(&mut buffer).expect("read file for SHA-256");
            if count == 0 {
                break;
            }
            digest.update(&buffer[..count]);
        }
        format!("{:x}", digest.finalize())
    }

    pub fn database(workspace: &Path) -> Connection {
        Connection::open(workspace.join("state.sqlite3")).expect("open workspace SQLite")
    }

    pub fn assert_sqlite_integrity(connection: &Connection) {
        let result: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .expect("SQLite integrity_check");
        assert_eq!(result, "ok");
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn object_workspace_cli_initializes_retries_and_adopts_explicitly() {
    let temporary = tempdir().expect("temporary directory");
    let fresh_workspace = temporary.path().join("fresh-workspace");
    let initialize_args = [
        "object-workspace",
        "initialize",
        "--workspace",
        fresh_workspace.to_str().expect("UTF-8 fresh workspace"),
        "--display-name",
        "Synthetic CLI workspace",
        "--migration-object-name-or-scope",
        "Synthetic customer readiness",
        "--creator-attribution",
        "CLI lifecycle proof",
    ];

    let initialized = support::success(support::cli().args(initialize_args));
    assert_eq!(
        initialized["schemaVersion"],
        "migration-stream/object-workspace/v1"
    );
    assert_eq!(initialized["establishedBy"], "INITIALIZED");
    assert_eq!(initialized["displayName"], "Synthetic CLI workspace");
    assert_eq!(
        initialized["migrationObjectNameOrScope"],
        "Synthetic customer readiness"
    );
    assert_eq!(initialized["creatorAttribution"], "CLI lifecycle proof");
    assert!(fresh_workspace.join("workspace.json").is_file());

    let exact_retry = support::success(support::cli().args(initialize_args));
    assert_eq!(exact_retry, initialized);
    let conflicting_retry = support::failure(support::cli().args([
        "object-workspace",
        "initialize",
        "--workspace",
        fresh_workspace.to_str().expect("UTF-8 fresh workspace"),
        "--display-name",
        "Different identity",
        "--migration-object-name-or-scope",
        "Synthetic customer readiness",
        "--creator-attribution",
        "CLI lifecycle proof",
    ]));
    assert_eq!(conflicting_retry["code"], "CONFLICT");

    let empty_adoption = temporary.path().join("empty-adoption");
    fs::create_dir(&empty_adoption).expect("empty adoption workspace");
    let rejected_adoption = support::failure(support::cli().args([
        "object-workspace",
        "adopt",
        "--workspace",
        empty_adoption.to_str().expect("UTF-8 empty workspace"),
        "--display-name",
        "Synthetic adopted workspace",
        "--migration-object-name-or-scope",
        "Synthetic customer readiness",
        "--creator-attribution",
        "CLI lifecycle proof",
    ]));
    assert_eq!(rejected_adoption["code"], "CONFLICT");

    let legacy_workspace = temporary.path().join("legacy-workspace");
    support::success(support::cli().args([
        "object-workspace",
        "initialize",
        "--workspace",
        legacy_workspace.to_str().expect("UTF-8 legacy workspace"),
        "--display-name",
        "Temporary legacy identity",
        "--migration-object-name-or-scope",
        "Synthetic customer readiness",
        "--creator-attribution",
        "CLI lifecycle fixture",
    ]));
    support::success(
        support::cli()
            .arg("ingest")
            .arg("--source")
            .arg(support::project_path("fixtures/packages/valid.zip"))
            .arg("--workspace")
            .arg(&legacy_workspace),
    );
    let database = support::database(&legacy_workspace);
    database
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TRIGGER object_workspace_engagement_bindings_no_update;
             DROP TRIGGER object_workspace_engagement_bindings_no_delete;
             DROP TABLE object_workspace_engagement_bindings;
             DROP TRIGGER object_workspace_bindings_no_update;
             DROP TRIGGER object_workspace_bindings_no_delete;
             DROP TABLE object_workspace_bindings;",
        )
        .expect("strip Object Workspace authority from copied legacy evidence");
    drop(database);
    let descriptor = legacy_workspace.join("workspace.json");
    support::make_writable(&descriptor);
    fs::remove_file(&descriptor).expect("remove copied descriptor");

    let adopt_args = [
        "object-workspace",
        "adopt",
        "--workspace",
        legacy_workspace.to_str().expect("UTF-8 legacy workspace"),
        "--display-name",
        "Synthetic adopted workspace",
        "--migration-object-name-or-scope",
        "Synthetic customer readiness",
        "--creator-attribution",
        "CLI lifecycle proof",
    ];
    let adopted = support::success(support::cli().args(adopt_args));
    assert_eq!(adopted["establishedBy"], "ADOPTED");
    assert_eq!(adopted["displayName"], "Synthetic adopted workspace");
    assert_eq!(
        support::database(&legacy_workspace)
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE state = 'COMPLETE'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("count preserved jobs"),
        1
    );
    assert_eq!(support::success(support::cli().args(adopt_args)), adopted);
}

#[test]
fn ingest_status_and_integrity_preserve_the_checked_in_package() {
    let temporary = tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    let source = support::project_path("fixtures/packages/valid.zip");
    support::initialize_workspace(&workspace);

    let ingest = support::success(
        support::cli()
            .arg("ingest")
            .arg("--source")
            .arg(&source)
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(ingest["source_name"], "valid.zip");
    assert_eq!(ingest["source_hash"], VALID_PACKAGE_SHA256);
    assert_eq!(ingest["source_records"], 4);
    assert_eq!(ingest["stored_records"], 4);
    assert_eq!(ingest["pending_records"], 0);
    assert!(ingest["peak_rss_bytes"].as_u64().is_some_and(|rss| rss > 0));
    assert!(
        ingest["declared_buffer_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );
    assert_eq!(
        ingest["manifest"]["csv_paths"],
        json!(["customer_accounts.csv"])
    );

    let stored_archive = support::path_field(&ingest, "stored_archive");
    support::assert_read_only(&stored_archive);
    assert_eq!(support::sha256(&stored_archive), VALID_PACKAGE_SHA256);

    let status = support::success(
        support::cli()
            .arg("status")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(status["active_jobs"], 0);
    assert_eq!(status["complete_jobs"], 1);
    assert_eq!(status["jobs"][0]["state"], "COMPLETE");
    assert_eq!(status["jobs"][0]["source_hash"], VALID_PACKAGE_SHA256);
    assert_eq!(status["jobs"][0]["source_records"], 4);
    assert_eq!(status["jobs"][0]["stored_records"], 4);
    assert_eq!(status["jobs"][0]["pending_records"], 0);
    assert!(status["jobs"][0]["peak_rss_bytes"].as_u64().is_some());
    assert!(
        status["jobs"][0]["declared_buffer_bytes"]
            .as_u64()
            .is_some()
    );

    let integrity = support::success(
        support::cli()
            .arg("integrity")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(integrity["ok"], true);
    assert_eq!(integrity["sqlite_ok"], true);
    assert_eq!(integrity["checked_jobs"], 1);
    assert_eq!(integrity["checked_packages"], 1);
    assert_eq!(integrity["issues"], json!([]));

    let connection = support::database(&workspace);
    support::assert_sqlite_integrity(&connection);
    let header: String = connection
        .query_row("SELECT header_json FROM csv_files", [], |row| row.get(0))
        .expect("persisted CSV header");
    assert_eq!(
        header,
        r#"["migrationPartnerId","category","organizationName1","notes"]"#
    );
    let rows = source_rows(&connection);
    assert_eq!(
        rows,
        vec![
            (
                r#"["000001","2","Acme, S.A.","first line\nsecond line"]"#.to_owned(),
                2,
                3
            ),
            (r#"["000002","1","Αθήνα Α.Ε.",""]"#.to_owned(), 4, 4),
            (
                r#"["000003","2","東京株式会社","leading zero 0007"]"#.to_owned(),
                5,
                5
            ),
            (
                r#"["000004","2","","empty organization remains empty"]"#.to_owned(),
                6,
                6
            ),
        ]
    );
}

#[test]
fn integrity_failure_returns_a_failure_exit_status() {
    let temporary = tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    let source = support::project_path("fixtures/packages/valid.zip");
    support::initialize_workspace(&workspace);

    let ingest = support::success(
        support::cli()
            .arg("ingest")
            .arg("--source")
            .arg(&source)
            .arg("--workspace")
            .arg(&workspace),
    );
    let stored_archive = support::path_field(&ingest, "stored_archive");
    support::make_writable(&stored_archive);
    fs::write(&stored_archive, b"tampered").expect("tamper with test archive");

    let failure = support::failure(
        support::cli()
            .arg("integrity")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(failure["code"], "INTEGRITY_FAILED");
    assert!(failure["message"].as_str().is_some_and(|message| {
        message.contains("hash mismatch")
            || message.contains("package evidence verification failed")
    }));
}

fn source_rows(connection: &Connection) -> Vec<(String, i64, i64)> {
    let mut statement = connection
        .prepare(
            "SELECT fields_json, line_start, line_end
             FROM source_records ORDER BY source_ordinal",
        )
        .expect("prepare source-record query");
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query source records")
        .collect::<rusqlite::Result<_>>()
        .expect("collect source records")
}

#[test]
fn reviewed_draft_validates_but_cannot_be_required_as_approved() {
    let temporary = tempdir().expect("temporary directory");
    let schema = support::project_path("schemas/migration-object-profile.schema.json");
    let source =
        support::project_path("profiles/synthetic.customer-account.readiness/profile.example.json");
    let reviewed = temporary.path().join("reviewed-draft.json");
    fs::copy(&source, &reviewed).expect("copy reviewed draft");

    let validated = support::success(
        support::cli()
            .args(["profile", "validate"])
            .arg("--schema")
            .arg(&schema)
            .arg("--profile")
            .arg(&reviewed),
    );
    assert_eq!(
        validated["profile_id"],
        "synthetic.customer-account.readiness"
    );
    assert_eq!(validated["version"], "0.1.0");
    assert_eq!(validated["status"], "REVIEWED_DRAFT");
    assert_eq!(validated["entity_count"], 3);

    let rejected = support::failure(
        support::cli()
            .args(["profile", "require-approved"])
            .arg("--schema")
            .arg(&schema)
            .arg("--profile")
            .arg(&reviewed),
    );
    assert_eq!(rejected["code"], "PROFILE_NOT_APPROVED");
    assert!(
        rejected["message"]
            .as_str()
            .is_some_and(|message| message.contains("REVIEWED_DRAFT"))
    );
    assert_eq!(
        fs::read(&reviewed).expect("reviewed draft after CLI"),
        fs::read(source).expect("source profile")
    );
}

#[derive(Debug)]
struct SnapshotRow {
    id: String,
    artifact_name: String,
    immutable_path: String,
    sha256: String,
    kind: String,
    byte_size: i64,
    system: Option<String>,
    client: Option<String>,
    release: Option<String>,
}

#[derive(Debug)]
struct ParserRow {
    snapshot_id: String,
    name: String,
    version: String,
    options: String,
    outcome: String,
    catalogue_path: Option<String>,
    catalogue_sha256: Option<String>,
    evidence: String,
}

#[test]
#[allow(clippy::too_many_lines)]
fn metadata_import_is_deterministic_generated_draft_evidence_only() {
    let temporary = tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    let source = support::project_path("fixtures/metadata/sanitized-customer-account.edmx");
    let golden_path =
        support::project_path("fixtures/golden/sanitized-customer-account.catalogue.json");
    support::initialize_workspace(&workspace);

    let imported = support::success(
        support::cli()
            .args(["metadata", "import-edmx"])
            .arg("--source")
            .arg(&source)
            .arg("--workspace")
            .arg(&workspace)
            .args(["--system", "SYNTHETIC_ODATA_V4"])
            .args(["--client", "SYNTHETIC"])
            .args(["--release", "SYNTHETIC_SAMPLE"])
            .args([
                "--artifact",
                "fixtures/metadata/sanitized-customer-account.edmx",
            ]),
    );
    assert_eq!(imported["status"], "GENERATED_DRAFT");
    assert_eq!(imported["snapshotSha256"], EDMX_SNAPSHOT_SHA256);
    assert_eq!(imported["snapshotBytes"], 1816);
    assert_eq!(imported["cataloguePayloadSha256"], CATALOGUE_PAYLOAD_SHA256);
    assert_eq!(imported["parserName"], "migration-stream-edmx");
    assert_eq!(imported["parserVersion"], "1.1.0");

    let snapshot_path = support::path_field(&imported, "snapshotPath");
    let catalogue_path = support::path_field(&imported, "cataloguePath");
    support::assert_read_only(&snapshot_path);
    support::assert_read_only(&catalogue_path);
    assert_eq!(support::sha256(&snapshot_path), EDMX_SNAPSHOT_SHA256);
    assert_eq!(
        support::sha256(&catalogue_path),
        imported["catalogueFileSha256"]
            .as_str()
            .expect("catalogue file hash")
    );
    assert_eq!(
        fs::read(&catalogue_path).expect("persisted catalogue"),
        fs::read(&golden_path).expect("golden catalogue")
    );

    support::make_writable(&snapshot_path);
    support::make_writable(&catalogue_path);
    let stale_snapshot_partial =
        workspace.join("metadata/22222222-2222-4222-8222-222222222222.part");
    let stale_catalogue_partial =
        workspace.join("catalogues/33333333-3333-4333-8333-333333333333.part");
    fs::write(&stale_snapshot_partial, b"interrupted snapshot").expect("stale snapshot partial");
    fs::write(&stale_catalogue_partial, b"interrupted catalogue").expect("stale catalogue partial");

    let retried = support::success(
        support::cli()
            .args(["metadata", "import-edmx"])
            .arg("--source")
            .arg(&source)
            .arg("--workspace")
            .arg(&workspace)
            .args(["--system", "SYNTHETIC_ODATA_V4"])
            .args(["--client", "SYNTHETIC"])
            .args(["--release", "SYNTHETIC_SAMPLE"])
            .args([
                "--artifact",
                "fixtures/metadata/sanitized-customer-account.edmx",
            ]),
    );
    assert_eq!(retried["snapshotId"], imported["snapshotId"]);
    assert_eq!(retried["importedAt"], imported["importedAt"]);
    support::assert_read_only(&snapshot_path);
    support::assert_read_only(&catalogue_path);
    assert!(!stale_snapshot_partial.exists());
    assert!(!stale_catalogue_partial.exists());

    let golden: Value =
        serde_json::from_slice(&fs::read(golden_path).expect("golden bytes")).expect("golden JSON");
    let connection = support::database(&workspace);
    support::assert_sqlite_integrity(&connection);
    let snapshot = snapshot_row(&connection);
    assert_eq!(snapshot.id, imported["snapshotId"]);
    assert_eq!(
        snapshot.artifact_name,
        "fixtures/metadata/sanitized-customer-account.edmx"
    );
    assert_eq!(
        snapshot.immutable_path,
        relative_path(&workspace, &snapshot_path)
    );
    assert_eq!(snapshot.sha256, EDMX_SNAPSHOT_SHA256);
    assert_eq!(snapshot.kind, "odata_edmx");
    assert_eq!(snapshot.byte_size, 1816);
    assert_eq!(snapshot.system.as_deref(), Some("SYNTHETIC_ODATA_V4"));
    assert_eq!(snapshot.client.as_deref(), Some("SYNTHETIC"));
    assert_eq!(snapshot.release.as_deref(), Some("SYNTHETIC_SAMPLE"));

    let parser = parser_row(&connection);
    assert_eq!(parser.snapshot_id, snapshot.id);
    assert_eq!(parser.name, "migration-stream-edmx");
    assert_eq!(parser.version, "1.1.0");
    assert_eq!(parser.outcome, "SUCCEEDED_GENERATED_DRAFT");
    assert_eq!(
        parser.catalogue_path.as_deref(),
        Some(relative_path(&workspace, &catalogue_path).as_str())
    );
    assert_eq!(
        parser.catalogue_sha256.as_deref(),
        imported["catalogueFileSha256"].as_str()
    );
    assert_eq!(
        parse_json(&parser.options),
        golden["catalogue"]["parser"]["options"]
    );
    assert_eq!(parse_json(&parser.evidence), golden["evidence"]);
    let snapshot_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM metadata_snapshots", [], |row| {
            row.get(0)
        })
        .expect("metadata snapshot count");
    let parser_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM parser_runs", [], |row| row.get(0))
        .expect("parser run count");
    let cleanup_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM partial_cleanups", [], |row| {
            row.get(0)
        })
        .expect("partial cleanup count");
    assert_eq!(snapshot_count, 1);
    assert_eq!(parser_count, 1);
    #[cfg(windows)]
    assert_eq!(cleanup_count, 2);
    #[cfg(not(windows))]
    assert_eq!(cleanup_count, 0);
    drop(connection);

    let integrity = support::success(
        support::cli()
            .arg("integrity")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(integrity["ok"], true);
    assert_eq!(integrity["checked_metadata_snapshots"], 1);
    assert_eq!(integrity["checked_catalogues"], 1);
    #[cfg(windows)]
    assert_eq!(integrity["checked_partial_cleanups"], 2);
    #[cfg(not(windows))]
    assert_eq!(integrity["checked_partial_cleanups"], 0);
    assert_eq!(integrity["warnings"], json!([]));
    assert_no_metadata_approval_command();
}

fn snapshot_row(connection: &Connection) -> SnapshotRow {
    connection
        .query_row(
            "SELECT id, artifact_name, immutable_path, sha256, kind, byte_size,
                    system_context, client_context, release_context
             FROM metadata_snapshots",
            [],
            |row| {
                Ok(SnapshotRow {
                    id: row.get(0)?,
                    artifact_name: row.get(1)?,
                    immutable_path: row.get(2)?,
                    sha256: row.get(3)?,
                    kind: row.get(4)?,
                    byte_size: row.get(5)?,
                    system: row.get(6)?,
                    client: row.get(7)?,
                    release: row.get(8)?,
                })
            },
        )
        .expect("metadata snapshot row")
}

fn parser_row(connection: &Connection) -> ParserRow {
    connection
        .query_row(
            "SELECT snapshot_id, parser_name, parser_version, options_json, outcome,
                    catalogue_path, catalogue_sha256, evidence_json
             FROM parser_runs",
            [],
            |row| {
                Ok(ParserRow {
                    snapshot_id: row.get(0)?,
                    name: row.get(1)?,
                    version: row.get(2)?,
                    options: row.get(3)?,
                    outcome: row.get(4)?,
                    catalogue_path: row.get(5)?,
                    catalogue_sha256: row.get(6)?,
                    evidence: row.get(7)?,
                })
            },
        )
        .expect("parser run row")
}

fn relative_path(workspace: &Path, path: &Path) -> String {
    let canonical_workspace =
        fs::canonicalize(workspace).expect("canonical workspace evidence root");
    let canonical_path = fs::canonicalize(path).expect("canonical workspace evidence path");
    canonical_path
        .strip_prefix(canonical_workspace)
        .expect("workspace-relative evidence path")
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn parse_json(source: &str) -> Value {
    serde_json::from_str(source).expect("persisted JSON")
}

fn assert_no_metadata_approval_command() {
    let output = support::output(support::cli().args(["metadata", "--help"]));
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("UTF-8 CLI help");
    assert!(!help.to_ascii_lowercase().contains("approv"));
}

#[test]
fn metadata_integrity_rejects_forged_parser_registration() {
    let temporary = tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    let source = support::project_path("fixtures/metadata/sanitized-customer-account.edmx");
    support::initialize_workspace(&workspace);
    support::success(
        support::cli()
            .args(["metadata", "import-edmx"])
            .arg("--source")
            .arg(&source)
            .arg("--workspace")
            .arg(&workspace)
            .args(["--system", "SYNTHETIC_ODATA_V4"])
            .args(["--client", "SYNTHETIC"])
            .args(["--release", "SYNTHETIC_SAMPLE"])
            .args([
                "--artifact",
                "fixtures/metadata/sanitized-customer-account.edmx",
            ]),
    );

    let connection = support::database(&workspace);
    connection
        .execute(
            "UPDATE parser_runs SET parser_name = 'untrusted-parser'",
            [],
        )
        .expect("forge parser registration");
    drop(connection);

    let failure = support::failure(
        support::cli()
            .arg("integrity")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(failure["code"], "INTEGRITY_FAILED");
    assert!(
        failure["message"]
            .as_str()
            .is_some_and(|message| message.contains("parser identity/options"))
    );
}

#[cfg(any(unix, windows))]
#[test]
fn metadata_import_refuses_a_symlinked_evidence_directory() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir as symlink;

    let temporary = tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    let outside = temporary.path().join("outside");
    support::initialize_workspace(&workspace);
    fs::create_dir_all(&outside).expect("outside directory");
    symlink(&outside, workspace.join("metadata")).expect("symlink metadata directory");
    let source = support::project_path("fixtures/metadata/sanitized-customer-account.edmx");

    let failure = support::failure(
        support::cli()
            .args(["metadata", "import-edmx"])
            .arg("--source")
            .arg(&source)
            .arg("--workspace")
            .arg(&workspace)
            .args(["--system", "SYNTHETIC_ODATA_V4"])
            .args(["--client", "SYNTHETIC"])
            .args(["--release", "SYNTHETIC_SAMPLE"])
            .args([
                "--artifact",
                "fixtures/metadata/sanitized-customer-account.edmx",
            ]),
    );
    assert_eq!(failure["code"], "EDMX_PARSE");
    assert_eq!(fs::read_dir(&outside).expect("outside listing").count(), 0);
}

#[test]
fn integrity_reports_foreign_key_tampering() {
    let temporary = tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    let source = support::project_path("fixtures/packages/valid.zip");
    support::initialize_workspace(&workspace);
    support::success(
        support::cli()
            .arg("ingest")
            .arg("--source")
            .arg(&source)
            .arg("--workspace")
            .arg(&workspace),
    );

    let connection = support::database(&workspace);
    connection
        .pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys for tamper setup");
    connection
        .execute("DELETE FROM csv_files", [])
        .expect("orphan source records");
    drop(connection);

    let failure = support::failure(
        support::cli()
            .arg("integrity")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(failure["code"], "INTEGRITY_FAILED");
    assert!(
        failure["message"]
            .as_str()
            .is_some_and(|message| message.contains("foreign_key_check"))
    );
}

#[test]
fn integrity_rereads_manifest_and_ordered_csv_evidence() {
    let temporary = tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    let source = support::project_path("fixtures/packages/valid.zip");
    support::initialize_workspace(&workspace);
    support::success(
        support::cli()
            .arg("ingest")
            .arg("--source")
            .arg(&source)
            .arg("--workspace")
            .arg(&workspace),
    );

    let connection = support::database(&workspace);
    connection
        .execute(
            "UPDATE archive_entries SET crc32 = crc32 + 1 WHERE is_csv = 1",
            [],
        )
        .expect("tamper durable manifest CRC");
    connection
        .execute(
            "UPDATE source_records
             SET fields_json = '[\"forged\"]'
             WHERE source_ordinal = 1",
            [],
        )
        .expect("tamper durable source record");
    drop(connection);

    let failure = support::failure(
        support::cli()
            .arg("integrity")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(failure["code"], "INTEGRITY_FAILED");
    let message = failure["message"].as_str().expect("integrity message");
    assert!(message.contains("manifest row"));
    assert!(message.contains("CSV record 1 evidence differs"));
}

#[test]
fn integrity_rejects_stored_counts_above_source_counts() {
    let temporary = tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    let source = support::project_path("fixtures/packages/valid.zip");
    support::initialize_workspace(&workspace);
    support::success(
        support::cli()
            .arg("ingest")
            .arg("--source")
            .arg(&source)
            .arg("--workspace")
            .arg(&workspace),
    );

    let connection = support::database(&workspace);
    connection
        .execute(
            "UPDATE jobs
             SET state = 'INTERRUPTED', error_code = 'TEST_TAMPER',
                 stored_records = source_records + 1",
            [],
        )
        .expect("tamper record counts");
    drop(connection);

    let failure = support::failure(
        support::cli()
            .arg("integrity")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(failure["code"], "INTEGRITY_FAILED");
    assert!(
        failure["message"]
            .as_str()
            .is_some_and(|message| message.contains("exceeds source record count"))
    );
}

#[cfg(any(unix, windows))]
#[test]
fn integrity_rejects_a_symlinked_package() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_file as symlink;

    let temporary = tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    let source = support::project_path("fixtures/packages/valid.zip");
    support::initialize_workspace(&workspace);
    let ingest = support::success(
        support::cli()
            .arg("ingest")
            .arg("--source")
            .arg(&source)
            .arg("--workspace")
            .arg(&workspace),
    );
    let stored_archive = support::path_field(&ingest, "stored_archive");
    support::make_writable(&stored_archive);
    fs::remove_file(&stored_archive).expect("remove test package copy");
    symlink(&source, &stored_archive).expect("replace package with symlink");

    let failure = support::failure(
        support::cli()
            .arg("integrity")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(failure["code"], "INTEGRITY_FAILED");
    assert!(
        failure["message"]
            .as_str()
            .is_some_and(|message| message.contains("is not a regular file"))
    );
}

#[test]
fn hard_crash_recovers_to_interrupted_without_claiming_completion() {
    let temporary = tempdir().expect("temporary directory");
    let workspace = temporary.path().join("workspace");
    let policy_path = temporary.path().join("crash-policy.json");
    let source = support::project_path("fixtures/packages/valid.zip");
    support::initialize_workspace(&workspace);
    let policy = json!({
        "spool_buffer_bytes": 65536,
        "minimum_free_space_bytes": 0,
        "maximum_source_bytes": 536_870_912,
        "maximum_entries": 1000,
        "maximum_entry_path_bytes": 1024,
        "maximum_expanded_bytes": 268_435_456,
        "maximum_compression_ratio": 100.0,
        "maximum_csv_field_bytes": 1_048_576,
        "maximum_csv_record_bytes": 4_194_304,
        "maximum_csv_columns": 1_024,
        "maximum_csv_records": 1_000_000,
        "transaction_batch_rows": 1
    });
    fs::write(
        &policy_path,
        serde_json::to_vec_pretty(&policy).expect("serialize crash policy"),
    )
    .expect("write crash policy");

    let crashed = support::output(
        support::cli()
            .arg("ingest")
            .arg("--source")
            .arg(&source)
            .arg("--workspace")
            .arg(&workspace)
            .arg("--policy")
            .arg(&policy_path)
            .env("MIGRATION_STREAM_TEST_CRASH_AFTER_BATCHES", "1"),
    );
    assert!(!crashed.status.success());

    let recovery = support::success(
        support::cli()
            .arg("recover")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(recovery["recovered_jobs"], 1);
    assert_eq!(recovery["removed_partial_files"], 0);
    assert_eq!(recovery["quarantined_partial_files"], 0);
    assert_eq!(recovery["deleted_tombstones"], 0);
    assert_eq!(recovery["cleanup_pending"], json!([]));

    let repeated_recovery = support::success(
        support::cli()
            .arg("recover")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(repeated_recovery["recovered_jobs"], 0);
    assert_eq!(repeated_recovery["recovered_profiling_runs"], 0);
    assert_eq!(repeated_recovery["removed_partial_files"], 0);
    assert_eq!(repeated_recovery["quarantined_partial_files"], 0);
    assert_eq!(repeated_recovery["deleted_tombstones"], 0);
    assert_eq!(repeated_recovery["cleanup_pending"], json!([]));

    let status = support::success(
        support::cli()
            .arg("status")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(status["active_jobs"], 0);
    assert_eq!(status["complete_jobs"], 0);
    assert_eq!(status["interrupted_jobs"], 1);
    assert_eq!(status["jobs"][0]["state"], "INTERRUPTED");
    assert_ne!(status["jobs"][0]["state"], "COMPLETE");
    assert_eq!(status["jobs"][0]["error_code"], "INTERRUPTED_RECOVERED");

    let integrity = support::success(
        support::cli()
            .arg("integrity")
            .arg("--workspace")
            .arg(&workspace),
    );
    assert_eq!(integrity["ok"], true);
    assert_eq!(integrity["sqlite_ok"], true);
    assert_eq!(integrity["warnings"], json!([]));
    assert_eq!(integrity["issues"], json!([]));

    let connection = support::database(&workspace);
    support::assert_sqlite_integrity(&connection);
    let complete_jobs: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM jobs WHERE state = 'COMPLETE'",
            [],
            |row| row.get(0),
        )
        .expect("count complete jobs");
    assert_eq!(complete_jobs, 0);
    let recovered_events: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM job_events
             WHERE state = 'INTERRUPTED' AND error_code = 'INTERRUPTED_RECOVERED'",
            [],
            |row| row.get(0),
        )
        .expect("count recovered events");
    assert_eq!(recovered_events, 1);
}

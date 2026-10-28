//! Real verified-bundle loopback lifecycle proof. Run explicitly with --ignored
//! and `MIGRATION_STREAM_TEST_PROJECT_RUNTIME_BUNDLE`; no substitute runtime is used.

#![cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]

use std::{
    fs,
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use migration_stream::project_object::{PROJECT_FILENAME, open_project};
use serde_json::{Value, json};

fn record(name: &str, value: &Value) {
    if let Some(path) = std::env::var_os("MIGRATION_STREAM_TEST_PROJECT_RUNTIME_EVIDENCE_DIR") {
        let path = PathBuf::from(path);
        fs::create_dir_all(&path).unwrap();
        let mut evidence = value.clone();
        evidence["proofHost"] = json!({"os": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "qualification": "PROVISIONAL_NATIVE_CANDIDATE"});
        #[cfg(target_os = "macos")]
        {
            let host_version = Command::new("/usr/bin/sw_vers")
                .arg("-productVersion")
                .output()
                .unwrap();
            assert!(host_version.status.success());
            evidence["proofHost"]["version"] =
                json!(String::from_utf8(host_version.stdout).unwrap().trim());
        }
        fs::write(
            path.join(format!("{name}.json")),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
    }
}

// Runtime and project CLI processes use only fixed packaged executable paths.
fn packaged_command(bundle: &Path, member: &str) -> Command {
    let mut executable = bundle.join(member);
    if cfg!(windows) {
        executable.set_extension("exe");
    }
    let mut command = Command::new(executable);
    command.env_clear();
    #[cfg(unix)]
    command.env("PATH", "/usr/bin:/bin");
    #[cfg(windows)]
    {
        let system_root = migration_stream_windows_native::system_root_directory().unwrap();
        system_root.verify().unwrap();
        command.env("SystemRoot", system_root.environment_path());
    }
    command
}

fn project_cli(
    bundle: &Path,
    name: &str,
    arguments: &[&str],
    path: &Path,
    succeeds: bool,
) -> Value {
    let output = packaged_command(bundle, "bin/migration-stream")
        .args(arguments)
        .arg(path)
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        succeeds,
        "{name}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let document: Value = serde_json::from_slice(if succeeds {
        &output.stdout
    } else {
        &output.stderr
    })
    .unwrap();
    assert_eq!(document["ok"], succeeds);
    record(name, &document);
    document
}

fn create_pending(
    bundle: &Path,
    parent: &Path,
    name: &str,
) -> migration_stream::project_object::OpenProject {
    let created = project_cli(
        bundle,
        &format!("{name}-create"),
        &[
            "project",
            "create",
            "--display-name",
            "Synthetic native project",
            "--directory",
        ],
        &parent.join(name),
        true,
    );
    let descriptor = PathBuf::from(created["result"]["descriptorPath"].as_str().unwrap());
    let inspected = project_cli(
        bundle,
        &format!("{name}-inspect-pending"),
        &["project", "inspect", "--project"],
        &descriptor,
        true,
    );
    assert_eq!(created["result"], inspected["result"]);
    assert_eq!(
        inspected["result"]["requiredAction"],
        "INITIALIZE_WORKSPACE_EXPLICITLY"
    );
    open_project(descriptor).unwrap()
}

fn invoke(
    bundle: &Path,
    name: &str,
    operation: &str,
    flag: &str,
    path: &Path,
    port: u16,
    succeeds: bool,
) -> Value {
    let output = packaged_command(bundle, "libexec/migration-stream-local-runtime")
        .args([operation, flag])
        .arg(path)
        .args(["--port", &port.to_string()])
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        succeeds,
        "{name}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let document: Value = serde_json::from_slice(if succeeds {
        &output.stdout
    } else {
        &output.stderr
    })
    .unwrap();
    assert_eq!(document["ok"], succeeds);
    record(name, &document);
    document
}

struct OwnedCleanup {
    bundle: PathBuf,
    workspace: PathBuf,
    port: u16,
}

impl Drop for OwnedCleanup {
    fn drop(&mut self) {
        // Existing lifecycle authenticates ownership. No PID-only signal or
        // arbitrary kill fallback is permitted, including failed assertions.
        let _ = packaged_command(&self.bundle, "libexec/migration-stream-local-runtime")
            .args(["stop", "--workspace"])
            .arg(&self.workspace)
            .args(["--port", &self.port.to_string()])
            .output();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn assert_owned(report: &Value, project: &migration_stream::project_object::OpenProject) {
    let result = &report["result"];
    assert_eq!(result["classification"], "OWNED");
    assert_eq!(
        result["workspace"]["objectWorkspace"]["workspaceId"],
        project.descriptor.binding.as_ref().unwrap().workspace_id
    );
    assert_eq!(
        result["workspace"]["objectWorkspace"]["descriptorHash"],
        project.descriptor.binding.as_ref().unwrap().descriptor_hash
    );
    #[cfg(not(windows))]
    assert_eq!(
        result["workspace"]["canonicalPath"],
        project.workspace.root().to_str().unwrap()
    );
    #[cfg(windows)]
    {
        use migration_stream_windows_native::{
            RealDirectory, windows_workspace_filesystem_identity_sha256,
        };
        let directory = RealDirectory::capture(project.workspace.root()).unwrap();
        directory.verify().unwrap();
        assert_eq!(
            result["workspace"]["canonicalPath"],
            directory.path().to_str().unwrap()
        );
        assert_eq!(
            result["workspace"]["filesystemIdentitySha256"],
            windows_workspace_filesystem_identity_sha256(directory.identity())
        );
        directory.verify().unwrap();
    }
    assert_eq!(
        result["workspace"]["filesystemIdentitySha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        result["applicationIdentitySha256"].as_str().unwrap().len(),
        64
    );
    assert!(result["workbenchPid"].as_u64().unwrap() > 0);
    assert!(result["supervisorPid"].as_u64().unwrap() > 0);
}

fn frontend_is_real(port: u16) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut bytes = Vec::new();
    stream.take(1024 * 1024).read_to_end(&mut bytes).unwrap();
    let response = String::from_utf8_lossy(&bytes);
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("<html"));
}

#[test]
#[ignore = "requires an independently verified real runtime bundle containing the project bridge"]
#[allow(clippy::too_many_lines)]
fn bound_project_real_start_reuse_stop_reopen_and_foreign_isolation() {
    let bundle = PathBuf::from(
        std::env::var_os("MIGRATION_STREAM_TEST_PROJECT_RUNTIME_BUNDLE")
            .expect("verified project-bridge bundle required"),
    );
    let bundle = bundle.canonicalize().unwrap();
    let verify = packaged_command(&bundle, "libexec/migration-stream-local-runtime")
        .arg("verify")
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stderr)
    );
    record(
        "bundle-verification",
        &serde_json::from_slice::<Value>(&verify.stdout).unwrap(),
    );
    let temp = tempfile::tempdir().unwrap();
    let pending = create_pending(&bundle, temp.path(), "pending");
    let port_a = free_port();
    let rejected = invoke(
        &bundle,
        "pending-prerequisite",
        "start",
        "--project",
        &pending.descriptor_path,
        port_a,
        false,
    );
    assert_eq!(rejected["code"], "RUNTIME_CONFLICT");
    assert!(
        rejected["message"]
            .as_str()
            .unwrap()
            .contains("initialization and bind_project")
    );
    assert!(!pending.workspace.root().join("state.sqlite3").exists());
    let make_bound = |name: &str| {
        let project = create_pending(&bundle, temp.path(), name);
        let refused = project_cli(
            &bundle,
            &format!("{name}-bind-before-initialize"),
            &["project", "bind", "--project"],
            &project.descriptor_path,
            false,
        );
        assert_eq!(refused["code"], "CONFLICT");
        assert!(!project.workspace.root().join("state.sqlite3").exists());
        let initialized = project_cli(
            &bundle,
            &format!("{name}-explicit-initialize"),
            &[
                "object-workspace",
                "initialize",
                "--display-name",
                "Synthetic native project",
                "--migration-object-name-or-scope",
                "Synthetic local runtime",
                "--creator-attribution",
                "Runtime bridge proof",
                "--workspace",
            ],
            project.workspace.root(),
            true,
        );
        let ready = project_cli(
            &bundle,
            &format!("{name}-inspect-ready"),
            &["project", "inspect", "--project"],
            &project.descriptor_path,
            true,
        );
        assert_eq!(ready["result"]["requiredAction"], "BIND_PROJECT");
        let bound = project_cli(
            &bundle,
            &format!("{name}-explicit-bind"),
            &["project", "bind", "--project"],
            &project.descriptor_path,
            true,
        );
        assert_eq!(
            bound["result"]["descriptor"]["binding"]["workspaceId"],
            initialized["result"]["workspaceId"]
        );
        let descriptor = PathBuf::from(bound["result"]["descriptorPath"].as_str().unwrap());
        let inspected = project_cli(
            &bundle,
            &format!("{name}-inspect-bound"),
            &["project", "inspect", "--project"],
            &descriptor,
            true,
        );
        assert_eq!(bound["result"], inspected["result"]);
        open_project(descriptor).unwrap()
    };
    let a = make_bound("a");
    let b = make_bound("b");
    let mut port_b = free_port();
    while port_b == port_a {
        port_b = free_port();
    }
    let _cleanup_a = OwnedCleanup {
        bundle: bundle.clone(),
        workspace: a.workspace.root().to_path_buf(),
        port: port_a,
    };
    let _cleanup_b = OwnedCleanup {
        bundle: bundle.clone(),
        workspace: b.workspace.root().to_path_buf(),
        port: port_b,
    };
    let authority_a = fs::read(a.workspace.root().join("workspace.json")).unwrap();
    let descriptor_a = fs::read(&a.descriptor_path).unwrap();
    let started = invoke(
        &bundle,
        "a-start",
        "start",
        "--project",
        &a.descriptor_path,
        port_a,
        true,
    );
    assert_owned(&started, &a);
    frontend_is_real(port_a);
    let reused = invoke(
        &bundle,
        "a-reuse",
        "start",
        "--project",
        &a.descriptor_path,
        port_a,
        true,
    );
    assert_owned(&reused, &a);
    for key in [
        "supervisorPid",
        "workbenchPid",
        "sessionDirectory",
        "applicationIdentitySha256",
    ] {
        assert_eq!(started["result"][key], reused["result"][key]);
    }
    let legacy = invoke(
        &bundle,
        "workspace-mode-compatible",
        "status",
        "--workspace",
        a.workspace.root(),
        port_a,
        true,
    );
    assert_eq!(
        legacy["result"]["workbenchPid"],
        started["result"]["workbenchPid"]
    );
    let wrong_port = invoke(
        &bundle,
        "b-cannot-take-a-listener",
        "start",
        "--project",
        &b.descriptor_path,
        port_a,
        false,
    );
    assert_eq!(wrong_port["code"], "RUNTIME_CONFLICT");
    let mut mismatched = a.descriptor.clone();
    mismatched.binding.clone_from(&b.descriptor.binding);
    fs::write(&a.descriptor_path, serde_json::to_vec(&mismatched).unwrap()).unwrap();
    let mismatch = invoke(
        &bundle,
        "mismatched-project-rejected",
        "start",
        "--project",
        &a.descriptor_path,
        port_a,
        false,
    );
    fs::write(&a.descriptor_path, &descriptor_a).unwrap();
    assert_eq!(mismatch["code"], "INTEGRITY_FAILED");
    let still_a = invoke(
        &bundle,
        "a-survives-mismatch",
        "status",
        "--project",
        &a.descriptor_path,
        port_a,
        true,
    );
    assert_eq!(
        still_a["result"]["workbenchPid"],
        started["result"]["workbenchPid"]
    );
    let b_started = invoke(
        &bundle,
        "b-start",
        "start",
        "--project",
        &b.descriptor_path,
        port_b,
        true,
    );
    assert_owned(&b_started, &b);
    assert_ne!(
        started["result"]["workbenchPid"],
        b_started["result"]["workbenchPid"]
    );
    assert_ne!(
        started["result"]["sessionDirectory"],
        b_started["result"]["sessionDirectory"]
    );
    let stopped = invoke(
        &bundle,
        "a-stop",
        "stop",
        "--project",
        &a.descriptor_path,
        port_a,
        true,
    );
    assert_eq!(stopped["result"]["classification"], "STOPPED");
    assert!(TcpStream::connect(("127.0.0.1", port_a)).is_err());
    let b_alive = invoke(
        &bundle,
        "b-remains-owned",
        "status",
        "--project",
        &b.descriptor_path,
        port_b,
        true,
    );
    assert_eq!(
        b_alive["result"]["workbenchPid"],
        b_started["result"]["workbenchPid"]
    );
    // A new CLI process resolves the same descriptor again after stop.
    let reopened = invoke(
        &bundle,
        "a-reopen",
        "start",
        "--project",
        &a.descriptor_path,
        port_a,
        true,
    );
    assert_owned(&reopened, &a);
    assert_ne!(
        reopened["result"]["workbenchPid"],
        started["result"]["workbenchPid"]
    );
    assert_eq!(
        reopened["result"]["applicationIdentitySha256"],
        started["result"]["applicationIdentitySha256"]
    );
    let restarted = invoke(
        &bundle,
        "a-project-restart",
        "restart",
        "--project",
        &a.descriptor_path,
        port_a,
        true,
    );
    assert_owned(&restarted, &a);
    assert_ne!(
        restarted["result"]["workbenchPid"],
        reopened["result"]["workbenchPid"]
    );
    assert_eq!(
        restarted["result"]["applicationIdentitySha256"],
        reopened["result"]["applicationIdentitySha256"]
    );
    invoke(
        &bundle,
        "a-final-stop",
        "stop",
        "--project",
        &a.descriptor_path,
        port_a,
        true,
    );
    let foreign = TcpListener::bind(("127.0.0.1", port_a)).unwrap();
    let foreign_status = invoke(
        &bundle,
        "foreign-status",
        "status",
        "--project",
        &a.descriptor_path,
        port_a,
        true,
    );
    assert_eq!(
        foreign_status["result"]["classification"],
        "FOREIGN_LISTENER"
    );
    for operation in ["start", "stop"] {
        let error = invoke(
            &bundle,
            &format!("foreign-{operation}-rejected"),
            operation,
            "--project",
            &a.descriptor_path,
            port_a,
            false,
        );
        assert_eq!(error["code"], "RUNTIME_CONFLICT");
        assert_eq!(foreign.local_addr().unwrap().port(), port_a);
        assert!(TcpStream::connect(("127.0.0.1", port_a)).is_ok());
    }
    drop(foreign);
    let b_stopped = invoke(
        &bundle,
        "b-final-stop",
        "stop",
        "--project",
        &b.descriptor_path,
        port_b,
        true,
    );
    assert_eq!(b_stopped["result"]["classification"], "STOPPED");
    assert!(TcpStream::connect(("127.0.0.1", port_b)).is_err());
    let integrity = invoke(
        &bundle,
        "a-stopped-integrity",
        "integrity",
        "--project",
        &a.descriptor_path,
        port_a,
        true,
    );
    assert_eq!(integrity["result"]["integrityResult"]["ok"], true);
    assert_eq!(
        authority_a,
        fs::read(a.workspace.root().join("workspace.json")).unwrap()
    );
    assert_eq!(descriptor_a, fs::read(a.descriptor_path).unwrap());
    assert!(
        a.workspace
            .root()
            .parent()
            .unwrap()
            .join(PROJECT_FILENAME)
            .is_file()
    );
    record(
        "proof-summary",
        &json!({"portA": port_a, "portB": port_b, "state": "ALL_OWNED_RUNTIMES_STOPPED", "bundle": bundle}),
    );
}

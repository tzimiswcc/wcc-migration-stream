use std::{path::PathBuf, process::ExitCode};

use clap::{Args, Parser, Subcommand};
use migration_stream::{
    KernelError,
    project_runtime::BoundProjectRuntime,
    runtime_bundle::{compiled_bundle_build_identity, verify_bundle_root},
    runtime_lifecycle::{
        DEFAULT_ASSISTED_RUNTIME_PORT, RuntimeCommandOptions, RuntimeSelection, integrity_runtime,
        restart_runtime, run_supervisor, start_runtime, status_runtime, stop_runtime,
    },
};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "migration-stream-local-runtime",
    about = "Verify and operate one assisted local Migration Stream runtime"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Verify every immutable member of this exact bundle.
    Verify,
    /// Start or report the exact owned loopback runtime.
    Start(LifecycleArguments),
    /// Open exactly one already-bound project through the existing runtime start.
    Open(ProjectOpenArguments),
    /// Classify the exact owned, stopped, stale, or foreign runtime state.
    Status(LifecycleArguments),
    /// Stop and start the exact workspace runtime after independent checks.
    Restart(LifecycleArguments),
    /// Run the exact bundled global integrity replay while stopped.
    Integrity(LifecycleArguments),
    /// Stop only an authenticated exact owned runtime.
    Stop(LifecycleArguments),
    /// Internal session-local supervisor.
    #[command(name = "__supervise", hide = true)]
    Supervise {
        #[arg(long)]
        request: PathBuf,
    },
    /// Internal build-provenance handshake used by the bundle assembler.
    #[command(name = "__build-identity", hide = true)]
    BuildIdentity,
}

#[derive(Clone, Debug, Args)]
struct LifecycleArguments {
    /// Existing real workspace directory selected only at process startup.
    #[arg(long, required_unless_present = "project", conflicts_with = "project")]
    workspace: Option<PathBuf>,
    /// Already-bound project descriptor selected by this trusted local caller.
    #[arg(
        long,
        required_unless_present = "workspace",
        conflicts_with = "workspace"
    )]
    project: Option<PathBuf>,
    /// Literal 127.0.0.1 port; the host is not configurable.
    #[arg(long, default_value_t = DEFAULT_ASSISTED_RUNTIME_PORT)]
    port: u16,
}

#[derive(Clone, Debug, Args)]
struct ProjectOpenArguments {
    /// One final bound project descriptor selected by the trusted native caller.
    #[arg(value_name = "PROJECT")]
    project: PathBuf,
    /// Literal 127.0.0.1 port; the host is not configurable.
    #[arg(long, default_value_t = DEFAULT_ASSISTED_RUNTIME_PORT)]
    port: u16,
}

impl ProjectOpenArguments {
    fn select(&self) -> migration_stream::Result<BoundProjectRuntime> {
        BoundProjectRuntime::open(&self.project, self.port)
    }
}

#[derive(Clone, Copy)]
enum LifecycleOperation {
    Start,
    Status,
    Restart,
    Integrity,
    Stop,
}

async fn execute_lifecycle(
    operation: LifecycleOperation,
    arguments: LifecycleArguments,
) -> migration_stream::Result<serde_json::Value> {
    if let Some(project) = arguments.project {
        let selection = BoundProjectRuntime::open(project, arguments.port)?;
        execute_selected(operation, &selection).await
    } else {
        let workspace = arguments.workspace.ok_or_else(|| {
            KernelError::RuntimeConflict("workspace or bound project is required".to_owned())
        })?;
        execute_selected(
            operation,
            &RuntimeCommandOptions {
                workspace,
                port: arguments.port,
            },
        )
        .await
    }
}

async fn execute_selected(
    operation: LifecycleOperation,
    selection: &impl RuntimeSelection,
) -> migration_stream::Result<serde_json::Value> {
    Ok(match operation {
        LifecycleOperation::Start => serde_json::to_value(start_runtime(selection).await?)?,
        LifecycleOperation::Status => serde_json::to_value(status_runtime(selection)?)?,
        LifecycleOperation::Restart => serde_json::to_value(restart_runtime(selection).await?)?,
        LifecycleOperation::Integrity => serde_json::to_value(integrity_runtime(selection)?)?,
        LifecycleOperation::Stop => serde_json::to_value(stop_runtime(selection).await?)?,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Success<T: Serialize> {
    schema_version: &'static str,
    ok: bool,
    result: T,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Failure {
    schema_version: &'static str,
    ok: bool,
    code: &'static str,
    message: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = execute(cli.command).await;
    match result {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&Success {
                    schema_version: "migration-stream/assisted-local-runtime/cli/v1",
                    ok: true,
                    result: value,
                })
                .expect("serializing runtime output cannot fail")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&Failure {
                    schema_version: "migration-stream/assisted-local-runtime/cli/v1",
                    ok: false,
                    code: error.code(),
                    message: error.to_string(),
                })
                .expect("serializing runtime failure cannot fail")
            );
            ExitCode::FAILURE
        }
    }
}

async fn execute(command: Command) -> migration_stream::Result<serde_json::Value> {
    match command {
        Command::Verify => Ok(serde_json::to_value(verify_bundle_root(bundle_root()?)?)?),
        Command::Start(arguments) => execute_lifecycle(LifecycleOperation::Start, arguments).await,
        Command::Open(arguments) => {
            let selection = arguments.select()?;
            execute_selected(LifecycleOperation::Start, &selection).await
        }
        Command::Status(arguments) => {
            execute_lifecycle(LifecycleOperation::Status, arguments).await
        }
        Command::Restart(arguments) => {
            execute_lifecycle(LifecycleOperation::Restart, arguments).await
        }
        Command::Integrity(arguments) => {
            execute_lifecycle(LifecycleOperation::Integrity, arguments).await
        }
        Command::Stop(arguments) => execute_lifecycle(LifecycleOperation::Stop, arguments).await,
        Command::Supervise { request } => {
            run_supervisor(&request).await?;
            Ok(serde_json::json!({
                "state": "STOPPED"
            }))
        }
        Command::BuildIdentity => Ok(serde_json::to_value(compiled_bundle_build_identity()?)?),
    }
}

#[cfg(not(windows))]
fn bundle_root() -> migration_stream::Result<PathBuf> {
    let executable = std::env::current_exe()
        .map_err(|error| KernelError::RuntimeUnavailable(format!("current executable: {error}")))?
        .canonicalize()
        .map_err(|error| KernelError::RuntimeUnavailable(format!("current executable: {error}")))?;
    let libexec = executable
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "libexec"))
        .ok_or_else(|| {
            KernelError::RuntimeUnavailable(
                "runtime controller is not inside the fixed bundle libexec directory".to_owned(),
            )
        })?;
    libexec
        .parent()
        .map(PathBuf::from)
        .ok_or_else(|| KernelError::RuntimeUnavailable("bundle root is absent".to_owned()))
}

#[cfg(windows)]
fn bundle_root() -> migration_stream::Result<PathBuf> {
    let executable = std::env::current_exe()
        .map_err(|error| KernelError::RuntimeUnavailable(format!("current executable: {error}")))?
        .canonicalize()
        .map_err(|error| KernelError::RuntimeUnavailable(format!("current executable: {error}")))?;
    let parent = executable.parent().ok_or_else(|| {
        KernelError::RuntimeUnavailable("runtime controller has no parent".to_owned())
    })?;
    match executable.file_name().and_then(|name| name.to_str()) {
        Some("migration-stream-local.exe") => Ok(parent.to_path_buf()),
        Some("migration-stream-local-runtime.exe")
            if parent.file_name().is_some_and(|name| name == "libexec") =>
        {
            parent
                .parent()
                .map(PathBuf::from)
                .ok_or_else(|| KernelError::RuntimeUnavailable("bundle root is absent".to_owned()))
        }
        _ => Err(KernelError::RuntimeUnavailable(
            "runtime command is not an exact fixed Windows bundle entrypoint".to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_accepts_existing_workspace_or_project_but_never_both() {
        for flag in ["--workspace", "--project"] {
            assert!(
                Cli::try_parse_from([
                    "runtime",
                    "start",
                    flag,
                    "/synthetic/path",
                    "--port",
                    "43210"
                ])
                .is_ok()
            );
        }
        assert!(Cli::try_parse_from(["runtime", "start"]).is_err());
        assert!(
            Cli::try_parse_from([
                "runtime",
                "start",
                "--workspace",
                "/synthetic/workspace",
                "--project",
                "/synthetic/project.mstream"
            ])
            .is_err()
        );
    }

    fn document_arguments(path: &std::ffi::OsStr, port: Option<u16>) -> ProjectOpenArguments {
        let mut arguments = vec![std::ffi::OsString::from("runtime"), "open".into()];
        if let Some(port) = port {
            arguments.extend(["--port".into(), port.to_string().into()]);
        }
        arguments.extend(["--".into(), path.to_os_string()]);
        match Cli::try_parse_from(arguments).unwrap().command {
            Command::Open(arguments) => arguments,
            _ => panic!("expected document-only open command"),
        }
    }

    #[test]
    fn open_preserves_one_native_document_argument_and_existing_port_default() {
        for path in [
            "customer readiness/project.mstream",
            "πελάτης/project.mstream",
            "literal $(touch nothing); & 'quoted'/project.mstream",
            "--workspace",
        ] {
            let arguments = document_arguments(std::ffi::OsStr::new(path), None);
            assert_eq!(arguments.project, PathBuf::from(path));
            assert_eq!(arguments.port, DEFAULT_ASSISTED_RUNTIME_PORT);
        }
        let arguments = document_arguments(std::ffi::OsStr::new("project.mstream"), Some(43210));
        assert_eq!(arguments.port, 43210);
    }

    #[cfg(unix)]
    #[test]
    fn open_parser_preserves_non_utf8_native_path_bytes() {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
        let native = std::ffi::OsString::from_vec(b"synthetic-\xff/project.mstream".to_vec());
        let arguments = document_arguments(&native, None);
        assert_eq!(arguments.project.as_os_str().as_bytes(), native.as_bytes());
    }

    #[test]
    fn open_rejects_missing_extra_documents_workspace_and_unexpected_flags() {
        for arguments in [
            vec!["runtime", "open"],
            vec!["runtime", "open", "--", "one.mstream", "two.mstream"],
            vec!["runtime", "open", "--workspace", "workspace", "one.mstream"],
            vec!["runtime", "open", "--project", "one.mstream"],
            vec!["runtime", "open", "--display-name", "name", "one.mstream"],
            vec![
                "runtime",
                "open",
                "--command",
                "command.json",
                "one.mstream",
            ],
            vec!["runtime", "open", "--port", "not-a-port", "one.mstream"],
        ] {
            assert!(Cli::try_parse_from(arguments).is_err());
        }
    }

    fn synthetic_bound_project(
        root: &std::path::Path,
    ) -> migration_stream::project_object::OpenProject {
        let pending =
            migration_stream::project_object::create_project(root, "Synthetic open document")
                .unwrap();
        migration_stream::object_workspace::establish_for_local_automation(
            pending.workspace.root(),
            "Synthetic open document",
            "Synthetic selection only",
            "Document-open proof operator",
        )
        .unwrap();
        migration_stream::project_object::bind_project(pending.descriptor_path).unwrap()
    }

    fn snapshot(root: &std::path::Path) -> std::collections::BTreeMap<PathBuf, String> {
        fn walk(
            root: &std::path::Path,
            path: &std::path::Path,
            entries: &mut std::collections::BTreeMap<PathBuf, String>,
        ) {
            for entry in std::fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let metadata = std::fs::symlink_metadata(&path).unwrap();
                let value = if metadata.file_type().is_symlink() {
                    format!("link:{:?}", std::fs::read_link(&path).unwrap())
                } else if metadata.is_dir() {
                    "directory".to_owned()
                } else {
                    migration_stream::canonical::sha256_hex(std::fs::read(&path).unwrap())
                };
                entries.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    format!("{value}:readonly={}", metadata.permissions().readonly()),
                );
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    walk(root, &path, entries);
                }
            }
        }
        let mut entries = std::collections::BTreeMap::new();
        walk(root, root, &mut entries);
        entries
    }

    fn assert_open_selection_refused(
        root: &std::path::Path,
        document: &std::path::Path,
        port: u16,
    ) {
        let before = snapshot(root);
        assert!(
            document_arguments(document.as_os_str(), Some(port))
                .select()
                .is_err()
        );
        assert_eq!(snapshot(root), before);
    }

    #[test]
    fn open_selects_only_exact_bound_workspace_without_starting_runtime_or_mutating() {
        let temp = tempfile::tempdir().unwrap();
        let project = synthetic_bound_project(&temp.path().join("customer readiness πελάτης"));
        let other = synthetic_bound_project(&temp.path().join("other"));
        let before = snapshot(temp.path());
        let selected = document_arguments(project.descriptor_path.as_os_str(), Some(43210))
            .select()
            .unwrap();
        assert_eq!(selected.project_path(), project.descriptor_path);
        assert_eq!(
            selected.runtime_options().workspace,
            project.workspace.root()
        );
        assert_eq!(selected.runtime_options().port, 43210);
        assert_eq!(
            selected.expected_workspace().object_workspace.as_ref(),
            project.lifecycle.workspace.as_ref()
        );
        assert_ne!(
            selected
                .expected_workspace()
                .object_workspace
                .as_ref()
                .unwrap()
                .workspace_id,
            other.lifecycle.workspace.as_ref().unwrap().workspace_id
        );
        assert_eq!(snapshot(temp.path()), before);
        // Deliberately stop at selection: execute_selected/start_runtime is not called.
    }

    #[test]
    fn open_refuses_pending_missing_mismatched_and_hardlinked_descriptors_without_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let pending = migration_stream::project_object::create_project(
            temp.path().join("pending"),
            "Pending open proof",
        )
        .unwrap();
        assert_open_selection_refused(temp.path(), &pending.descriptor_path, 4173);
        assert!(!pending.workspace.root().join("state.sqlite3").exists());
        assert_open_selection_refused(
            temp.path(),
            &temp.path().join("missing/project.mstream"),
            4173,
        );
        let project = synthetic_bound_project(&temp.path().join("bound"));
        assert_open_selection_refused(temp.path(), &project.descriptor_path, 0);
        let other = synthetic_bound_project(&temp.path().join("other"));
        let original = std::fs::read(&project.descriptor_path).unwrap();
        let mut changed = project.descriptor.clone();
        changed.binding = other.descriptor.binding;
        std::fs::write(
            &project.descriptor_path,
            serde_json::to_vec(&changed).unwrap(),
        )
        .unwrap();
        assert_open_selection_refused(temp.path(), &project.descriptor_path, 4173);
        std::fs::write(&project.descriptor_path, original).unwrap();
        std::fs::hard_link(
            &project.descriptor_path,
            temp.path().join("descriptor-alias"),
        )
        .unwrap();
        assert_open_selection_refused(temp.path(), &project.descriptor_path, 4173);
    }

    #[cfg(unix)]
    #[test]
    fn open_refuses_symlinked_document_without_following_or_mutating_it() {
        let temp = tempfile::tempdir().unwrap();
        let project = synthetic_bound_project(&temp.path().join("bound"));
        let retained = temp.path().join("retained-descriptor");
        std::fs::rename(&project.descriptor_path, &retained).unwrap();
        std::os::unix::fs::symlink(&retained, &project.descriptor_path).unwrap();
        assert_open_selection_refused(temp.path(), &project.descriptor_path, 4173);
    }
}

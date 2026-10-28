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
}

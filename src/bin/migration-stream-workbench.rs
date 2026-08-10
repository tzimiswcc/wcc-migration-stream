use std::{io::Read as _, net::SocketAddr, path::PathBuf, process::ExitCode};

use clap::Parser;
#[cfg(unix)]
use migration_stream::engagement_workbench::assisted_runtime_session_token_is_valid;
use migration_stream::engagement_workbench::{
    AssistedRuntimeIdentityConfig, EngagementWorkbenchConfig,
    build_engagement_application_router_with_runtime_identity,
};
use migration_stream::runtime_bundle::compiled_bundle_build_identity;
use migration_stream::workbench::{
    DEFAULT_LISTEN_ADDRESS, WorkbenchConfig, build_application_router, validate_loopback_address,
};
#[cfg(unix)]
use serde::Serialize;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
#[cfg(unix)]
use tokio::{io::AsyncWriteExt as _, net::UnixStream};

#[derive(Debug, Parser)]
#[command(
    name = "migration-stream-workbench",
    about = "Local practitioner workbench or fresh assisted-engagement setup"
)]
#[allow(clippy::struct_excessive_bools)]
struct Arguments {
    /// Controlled Migration Stream workspace selected only at process startup.
    #[arg(long)]
    workspace: Option<PathBuf>,

    /// Internal build-provenance handshake used by the bundle assembler.
    #[arg(long, hide = true)]
    bundle_build_identity: bool,

    /// Exact job identity fixed for the process lifetime.
    #[arg(long)]
    job: Option<String>,

    /// Optional exact profiling-run identity fixed for the process lifetime.
    #[arg(long)]
    profiling_run: Option<String>,

    /// Create or reopen one isolated local engagement in the controlled workspace.
    #[arg(long)]
    engagement_setup: bool,

    /// Server-owned bounded intake policy for engagement setup.
    #[arg(long, default_value = "config/engagement-intake-policy.json")]
    engagement_intake_policy: PathBuf,

    /// Server-owned value-redacted profiling policy for engagement setup.
    #[arg(long, default_value = "config/engagement-profiling-policy.json")]
    engagement_profiling_policy: PathBuf,

    /// Server-owned validation policy for engagement setup.
    #[arg(long, default_value = "config/engagement-validation-policy.json")]
    engagement_validation_policy: PathBuf,

    /// Server-owned reviewed object-pack catalogue.
    #[arg(long, default_value = "object-packs")]
    object_packs: PathBuf,

    /// Canonical Migration Object Profile validation schema.
    #[arg(long, default_value = "schemas/migration-object-profile.schema.json")]
    profile_schema: PathBuf,

    /// Canonical source-mapping validation schema.
    #[arg(long, default_value = "schemas/source-mapping-version.schema.json")]
    mapping_schema: PathBuf,

    /// Explicit loopback listener. Non-loopback addresses are rejected.
    #[arg(long, default_value = "127.0.0.1:4173")]
    listen: SocketAddr,

    /// Trusted production frontend assets loaded into bounded memory at startup.
    #[arg(long, default_value = "web/dist")]
    assets: PathBuf,

    /// Enable only governed WP-005 issue decisions and correction overlays.
    #[arg(long)]
    enable_governed_decisions: bool,

    /// Enable bounded WP-006 local migration-result readiness approval.
    #[arg(long)]
    enable_migration_result_approval: bool,

    /// Enable governed local readiness-package publication.
    #[arg(long)]
    enable_readiness_publication: bool,

    /// Stop when the owning assisted-runtime supervisor closes standard input.
    #[arg(long, hide = true)]
    parent_watchdog: bool,

    /// Session-local readiness socket owned by the assisted runtime supervisor.
    #[arg(long, hide = true)]
    readiness_socket: Option<PathBuf>,

    /// Exact random token for the assisted-runtime readiness handshake.
    #[arg(long, hide = true)]
    runtime_session_token: Option<String>,
}

#[cfg(unix)]
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeReadiness {
    schema_version: &'static str,
    session_token: String,
    process_id: u32,
    listen_address: String,
    workspace_path: PathBuf,
    filesystem_identity_sha256: String,
}

#[tokio::main]
// Startup deliberately keeps argument-to-authority wiring in one auditable path.
#[allow(clippy::too_many_lines)]
async fn main() -> ExitCode {
    let arguments = Arguments::parse();
    if arguments.bundle_build_identity {
        return match compiled_bundle_build_identity() {
            Ok(identity) => {
                println!(
                    "{}",
                    serde_json::json!({
                        "schemaVersion": "migration-stream/workbench-build-identity/v1",
                        "ok": true,
                        "result": identity,
                    })
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("workbench build identity rejected: {}", error.code());
                ExitCode::FAILURE
            }
        };
    }
    let Some(workspace) = arguments.workspace else {
        eprintln!("workbench startup rejected: WORKSPACE_REQUIRED");
        return ExitCode::FAILURE;
    };
    if !runtime_readiness_arguments_are_valid(
        arguments.parent_watchdog,
        arguments.readiness_socket.as_deref(),
        arguments.runtime_session_token.as_deref(),
    ) {
        eprintln!("workbench startup rejected: RUNTIME_READINESS_INVALID");
        return ExitCode::FAILURE;
    }
    start_parent_watchdog(arguments.parent_watchdog);
    if validate_loopback_address(arguments.listen).is_err() {
        eprintln!("workbench startup rejected: LISTENER_NOT_LOOPBACK");
        return ExitCode::FAILURE;
    }
    let Ok(listener) = tokio::net::TcpListener::bind(arguments.listen).await else {
        eprintln!("workbench listener could not bind");
        return ExitCode::FAILURE;
    };
    let workspace_for_readiness = workspace.clone();
    let application = if arguments.engagement_setup {
        if arguments.job.is_some()
            || arguments.profiling_run.is_some()
            || arguments.enable_governed_decisions
            || arguments.enable_migration_result_approval
            || arguments.enable_readiness_publication
        {
            eprintln!("workbench startup rejected: ENGAGEMENT_MODE_CONFLICT");
            return ExitCode::FAILURE;
        }
        let config = EngagementWorkbenchConfig {
            workspace,
            intake_policy: arguments.engagement_intake_policy,
            profiling_policy: arguments.engagement_profiling_policy,
            validation_policy: arguments.engagement_validation_policy,
            object_packs: arguments.object_packs,
            profile_schema: arguments.profile_schema,
            mapping_schema: arguments.mapping_schema,
        };
        let assisted_runtime_identity =
            arguments
                .runtime_session_token
                .as_ref()
                .map(|session_token| AssistedRuntimeIdentityConfig {
                    session_token: session_token.clone(),
                    listen_address: arguments.listen.to_string(),
                });
        match build_engagement_application_router_with_runtime_identity(
            config,
            arguments.assets,
            assisted_runtime_identity.as_ref(),
        ) {
            Ok(application) => application,
            Err(error) => {
                eprintln!("workbench startup rejected: {}", error.code());
                return ExitCode::FAILURE;
            }
        }
    } else {
        let Some(job) = arguments.job else {
            eprintln!("workbench startup rejected: JOB_REQUIRED");
            return ExitCode::FAILURE;
        };
        let mut config = WorkbenchConfig::new(workspace, job, arguments.profiling_run)
            .with_profile_schema(arguments.profile_schema);
        if arguments.enable_governed_decisions {
            config = config.with_governed_decisions();
        }
        if arguments.enable_migration_result_approval {
            config = config.with_migration_result_approval();
        }
        if arguments.enable_readiness_publication {
            config = config.with_readiness_publication();
        }
        match build_application_router(config, arguments.assets) {
            Ok(application) => application,
            Err(error) => {
                eprintln!("workbench startup rejected: {}", error.code());
                return ExitCode::FAILURE;
            }
        }
    };
    let mode = if arguments.engagement_setup {
        "assisted-engagement-setup"
    } else if arguments.enable_readiness_publication {
        "readiness-publication"
    } else if arguments.enable_migration_result_approval {
        "migration-result-readiness"
    } else if arguments.enable_governed_decisions {
        "governed-decision"
    } else {
        "read-only"
    };
    println!(
        "Migration Stream {mode} workbench listening at http://{}",
        listener.local_addr().unwrap_or(DEFAULT_LISTEN_ADDRESS)
    );
    if let (Some(readiness_socket), Some(session_token)) = (
        arguments.readiness_socket.as_deref(),
        arguments.runtime_session_token.as_deref(),
    ) && emit_runtime_readiness(
        readiness_socket,
        session_token,
        arguments.listen,
        &workspace_for_readiness,
    )
    .await
    .is_err()
    {
        eprintln!("workbench startup rejected: RUNTIME_READINESS_FAILED");
        return ExitCode::FAILURE;
    }
    let server = axum::serve(listener, application).with_graceful_shutdown(shutdown_signal());
    if server.await.is_err() {
        eprintln!("workbench server stopped unexpectedly");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn runtime_readiness_arguments_are_valid(
    parent_watchdog: bool,
    readiness_socket: Option<&std::path::Path>,
    session_token: Option<&str>,
) -> bool {
    readiness_socket.is_some() == session_token.is_some()
        && (!parent_watchdog || readiness_socket.is_some())
}

#[cfg(windows)]
fn runtime_readiness_arguments_are_valid(
    parent_watchdog: bool,
    readiness_socket: Option<&std::path::Path>,
    _session_token: Option<&str>,
) -> bool {
    !parent_watchdog && readiness_socket.is_none()
}

#[cfg(not(any(unix, windows)))]
fn runtime_readiness_arguments_are_valid(
    parent_watchdog: bool,
    readiness_socket: Option<&std::path::Path>,
    session_token: Option<&str>,
) -> bool {
    !parent_watchdog && readiness_socket.is_none() && session_token.is_none()
}

fn start_parent_watchdog(enabled: bool) {
    if !enabled {
        return;
    }
    std::thread::spawn(|| {
        let mut byte = [0_u8; 1];
        loop {
            match std::io::stdin().read(&mut byte) {
                Ok(0) | Err(_) => std::process::exit(0),
                Ok(_) => {}
            }
        }
    });
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(unix)]
async fn emit_runtime_readiness(
    socket: &std::path::Path,
    session_token: &str,
    listen_address: SocketAddr,
    workspace: &std::path::Path,
) -> migration_stream::Result<()> {
    if !assisted_runtime_session_token_is_valid(session_token) {
        return Err(migration_stream::KernelError::RuntimeUnavailable(
            "runtime session token is invalid".to_owned(),
        ));
    }
    let canonical = workspace.canonicalize().map_err(|error| {
        migration_stream::KernelError::RuntimeUnavailable(format!(
            "workspace readiness identity: {error}"
        ))
    })?;
    let metadata = std::fs::symlink_metadata(&canonical).map_err(|error| {
        migration_stream::KernelError::RuntimeUnavailable(format!(
            "workspace readiness identity: {error}"
        ))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(migration_stream::KernelError::RuntimeUnavailable(
            "workspace readiness identity is not one real directory".to_owned(),
        ));
    }
    let readiness = RuntimeReadiness {
        schema_version: "migration-stream/assisted-local-runtime/readiness/v2",
        session_token: session_token.to_owned(),
        process_id: std::process::id(),
        listen_address: listen_address.to_string(),
        workspace_path: canonical,
        filesystem_identity_sha256:
            migration_stream::runtime_lifecycle::unix_workspace_filesystem_identity_sha256(
                metadata.dev(),
                metadata.ino(),
            ),
    };
    let bytes =
        migration_stream::canonical::canonical_json_bytes(&serde_json::to_value(readiness)?)?;
    let mut stream = UnixStream::connect(socket).await.map_err(|error| {
        migration_stream::KernelError::RuntimeUnavailable(format!(
            "runtime readiness connection failed: {error}"
        ))
    })?;
    stream.write_all(&bytes).await.map_err(|error| {
        migration_stream::KernelError::RuntimeUnavailable(format!(
            "runtime readiness write failed: {error}"
        ))
    })?;
    stream.shutdown().await.map_err(|error| {
        migration_stream::KernelError::RuntimeUnavailable(format!(
            "runtime readiness shutdown failed: {error}"
        ))
    })
}

#[cfg(not(unix))]
// Preserve the shared awaited readiness boundary; Windows reports readiness
// through its native supervisor channel and must fail closed on this Unix path.
#[allow(clippy::unused_async)]
async fn emit_runtime_readiness(
    _socket: &std::path::Path,
    _session_token: &str,
    _listen_address: SocketAddr,
    _workspace: &std::path::Path,
) -> migration_stream::Result<()> {
    Err(migration_stream::KernelError::BundlePlatform(
        "assisted runtime readiness currently supports Unix hosts only".to_owned(),
    ))
}

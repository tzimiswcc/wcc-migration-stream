//! Owned lifecycle for the assisted local runtime bundle.
//!
//! Public commands derive every executable and static dependency from the
//! verified bundle. A short-lived launcher talks to one session-local
//! supervisor over a mode-0600 Unix socket; the supervisor retains and reaps
//! the workbench child. PIDs are diagnostic and are never sole kill authority.

use std::{
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

use crate::{
    object_workspace::{ObjectWorkspaceRecord, ObjectWorkspaceState},
    runtime_bundle::BundleVerification,
};

#[cfg(not(any(unix, windows)))]
use crate::{KernelError, Result};

pub const DEFAULT_ASSISTED_RUNTIME_PORT: u16 = 4173;
pub const RUNTIME_STATUS_SCHEMA: &str = "migration-stream/assisted-local-runtime/status/v2";
pub const RUNTIME_INTEGRITY_SCHEMA: &str = "migration-stream/assisted-local-runtime/integrity/v2";

/// Domain-separated full filesystem-object identity for Unix diagnostic
/// coordinates. The digest, rather than either numeric component alone, is
/// native-object authority. It intentionally excludes the mutable path: the
/// canonical path is a separate mandatory runtime startup-scope binding.
#[cfg(unix)]
#[must_use]
pub fn unix_workspace_filesystem_identity_sha256(device: u64, inode: u64) -> String {
    use sha2::{Digest as _, Sha256};

    let mut digest = Sha256::new();
    digest.update(b"migration-stream/workspace-filesystem-identity/v1\0unix\0");
    digest.update(device.to_be_bytes());
    digest.update(inode.to_be_bytes());
    format!("{:x}", digest.finalize())
}

#[derive(Clone, Debug)]
pub struct RuntimeCommandOptions {
    pub workspace: PathBuf,
    pub port: u16,
}

impl RuntimeCommandOptions {
    #[must_use]
    pub const fn listen_address(&self) -> SocketAddrV4 {
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, self.port)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuntimeClassification {
    Stopped,
    Starting,
    Owned,
    Stopping,
    Stale,
    ForeignListener,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatusReport {
    pub schema_version: &'static str,
    pub classification: RuntimeClassification,
    pub bundle: BundleVerification,
    pub workspace: RuntimeWorkspaceIdentity,
    pub listen_address: String,
    pub url: String,
    pub runtime_root: PathBuf,
    pub session_directory: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supervisor_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workbench_pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workbench_process_group_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub application_identity_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeWorkspaceIdentity {
    /// Required startup-selected location binding. It is distinct from the
    /// native object identity and a moved workspace requires reselection.
    pub canonical_path: PathBuf,
    /// Sole native filesystem-object identity.
    pub filesystem_identity_sha256: String,
    pub object_workspace_state: ObjectWorkspaceState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_workspace: Option<ObjectWorkspaceRecord>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeIntegrityReport {
    pub schema_version: &'static str,
    pub bundle: BundleVerification,
    pub workspace: RuntimeWorkspaceIdentity,
    pub bundled_cli_path: PathBuf,
    pub integrity_result: serde_json::Value,
}

#[cfg(unix)]
mod unix {
    use std::{
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        net::{Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpStream},
        os::unix::{
            fs::{
                DirBuilderExt as _, FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _,
                PermissionsExt as _,
            },
            net::UnixStream,
            process::CommandExt as _,
        },
        path::{Path, PathBuf},
        process::{Child, ChildStdin, Command, ExitStatus, Stdio},
        time::{Duration, Instant},
    };

    use chrono::Utc;
    use fs2::FileExt as _;
    use rustix::{
        fs::{Mode, OFlags, open},
        process::{
            Pid, Signal, WaitId, WaitIdOptions, geteuid, getpgid, kill_process, kill_process_group,
            test_kill_process_group, waitid,
        },
    };
    use serde::{Deserialize, Serialize, de::DeserializeOwned};
    use sha2::{Digest, Sha256};
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::UnixListener,
        time::{MissedTickBehavior, interval, sleep, timeout},
    };
    use uuid::Uuid;

    use super::{
        RUNTIME_INTEGRITY_SCHEMA, RUNTIME_STATUS_SCHEMA, RuntimeClassification,
        RuntimeCommandOptions, RuntimeIntegrityReport, RuntimeStatusReport,
        RuntimeWorkspaceIdentity,
    };
    use crate::{
        KernelError, Result,
        canonical::{canonical_json_bytes, parse_unique_json},
        object_workspace::ObjectWorkspaceService,
        runtime_bundle::{BundleVerification, verify_bundle_root},
    };

    const LAUNCH_REQUEST_SCHEMA: &str = "migration-stream/assisted-local-runtime/launch-request/v2";
    const SESSION_RECORD_SCHEMA: &str = "migration-stream/assisted-local-runtime/session/v2";
    const CONTROL_REQUEST_SCHEMA: &str =
        "migration-stream/assisted-local-runtime/control-request/v2";
    const CONTROL_RESPONSE_SCHEMA: &str =
        "migration-stream/assisted-local-runtime/control-response/v2";
    const APPLICATION_IDENTITY_SCHEMA: &str =
        "migration-stream/assisted-local-runtime/application-identity/v2";
    const RUNTIME_DIRECTORY_NAME: &str = "migration-stream-runtime";
    const SESSION_DIRECTORY_PREFIX: &str = "session-";
    const LAUNCH_REQUEST_FILENAME: &str = "launch-request.json";
    const SESSION_RECORD_FILENAME: &str = "session.json";
    const CONTROL_SOCKET_FILENAME: &str = "control.sock";
    const READINESS_SOCKET_FILENAME: &str = "readiness.sock";
    const COMMAND_LOCK_FILENAME: &str = "command.lock";
    const OWNER_LOCK_FILENAME: &str = "owner.lock";
    const SESSION_TEMP_DIRECTORY_NAME: &str = "tmp";
    const WORKBENCH_LOG_FILENAME: &str = "workbench.log";
    const SUPERVISOR_LOG_FILENAME: &str = "supervisor.log";
    const MAXIMUM_RUNTIME_JSON_BYTES: u64 = 128 * 1024;
    const MAXIMUM_RUNTIME_TEMP_ENTRIES: usize = 1_024;
    const MAXIMUM_RUNTIME_TEMP_DEPTH: usize = 8;
    const MAXIMUM_INTEGRITY_OUTPUT_BYTES: u64 = 4 * 1024 * 1024;
    const INTEGRITY_TIMEOUT: Duration = Duration::from_mins(2);
    const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
    const COMMAND_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
    const START_TIMEOUT: Duration = Duration::from_secs(30);
    const TERM_TIMEOUT: Duration = Duration::from_secs(5);
    const KILL_TIMEOUT: Duration = Duration::from_secs(5);
    const LISTENER_PROBE_TIMEOUT: Duration = Duration::from_millis(250);
    const LISTENER_QUIET_WINDOW: Duration = Duration::from_secs(1);
    const POLL_INTERVAL: Duration = Duration::from_millis(100);
    const MAXIMUM_UNIX_SOCKET_PATH_BYTES: usize = 100;

    #[derive(Clone, Debug)]
    struct RuntimeContext {
        bundle: BundleVerification,
        bundle_root: PathBuf,
        current_executable: PathBuf,
        workspace: RuntimeWorkspaceIdentity,
        listen_address: SocketAddrV4,
        runtime_root: PathBuf,
        session_directory: PathBuf,
        session_key: String,
    }

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct StableWorkspaceIdentity {
        canonical_path: PathBuf,
        filesystem_identity_sha256: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        descriptor_sha256: Option<String>,
    }

    impl RuntimeContext {
        fn url(&self) -> String {
            format!("http://{}", self.listen_address)
        }

        fn launch_request_path(&self) -> PathBuf {
            self.session_directory.join(LAUNCH_REQUEST_FILENAME)
        }

        fn control_socket_path(&self) -> PathBuf {
            self.session_directory.join(CONTROL_SOCKET_FILENAME)
        }

        fn command_lock_path(&self) -> PathBuf {
            self.session_directory.join(COMMAND_LOCK_FILENAME)
        }

        fn owner_lock_path(&self) -> PathBuf {
            self.session_directory.join(OWNER_LOCK_FILENAME)
        }

        fn supervisor_log_path(&self) -> PathBuf {
            self.session_directory.join(SUPERVISOR_LOG_FILENAME)
        }
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct LaunchRequest {
        schema_version: String,
        session_token: String,
        session_key: String,
        bundle_root: PathBuf,
        bundle_manifest_sha256: String,
        bundle_commit: String,
        workspace: StableWorkspaceIdentity,
        listen_address: String,
        url: String,
        runtime_root: PathBuf,
        session_directory: PathBuf,
        workbench_sha256: String,
        created_at: String,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct SessionRecord {
        schema_version: String,
        session_token_sha256: String,
        session_key: String,
        bundle_root: PathBuf,
        bundle_manifest_sha256: String,
        bundle_commit: String,
        workspace: StableWorkspaceIdentity,
        listen_address: String,
        url: String,
        supervisor_pid: u32,
        workbench_pid: u32,
        workbench_process_group_id: u32,
        workbench_sha256: String,
        log_path: PathBuf,
        state: SupervisorState,
        started_at: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        application_identity_sha256: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        failure: Option<String>,
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    enum SupervisorState {
        Starting,
        Ready,
        Stopping,
        Failed,
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "SCREAMING_SNAKE_CASE")]
    enum ControlOperation {
        Status,
        Stop,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct ControlRequest {
        schema_version: String,
        session_token: String,
        operation: ControlOperation,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct ControlResponse {
        schema_version: String,
        accepted: bool,
        state: SupervisorState,
        session_key: String,
        bundle_root: PathBuf,
        bundle_manifest_sha256: String,
        bundle_commit: String,
        workspace: StableWorkspaceIdentity,
        listen_address: String,
        url: String,
        supervisor_pid: u32,
        workbench_pid: u32,
        workbench_process_group_id: u32,
        workbench_sha256: String,
        log_path: PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        application_identity_sha256: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    }

    #[derive(Clone, Debug, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct RuntimeReadiness {
        schema_version: String,
        session_token: String,
        process_id: u32,
        listen_address: String,
        workspace_path: PathBuf,
        filesystem_identity_sha256: String,
    }

    #[derive(Clone, Debug, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct RuntimeHttpIdentity {
        schema_version: String,
        session_token_sha256: String,
        process_id: u32,
        listen_address: String,
        workspace_path: PathBuf,
        filesystem_identity_sha256: String,
        application_identity_sha256: String,
    }

    struct ClassifiedRuntime {
        classification: RuntimeClassification,
        request: Option<LaunchRequest>,
        response: Option<ControlResponse>,
        reason: Option<String>,
    }

    struct SocketPathGuard(PathBuf);

    impl Drop for SocketPathGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    /// Verify and start or idempotently report one exact local runtime.
    ///
    /// # Errors
    ///
    /// Returns an error when bundle, workspace, process, listener, or session
    /// authority cannot be verified or the bounded startup does not complete.
    pub async fn start_runtime(options: &RuntimeCommandOptions) -> Result<RuntimeStatusReport> {
        let context = resolve_context(options)?;
        ensure_runtime_directories(&context)?;
        let _lock = lock_command(&context)?;
        start_runtime_locked(&context).await
    }

    async fn start_runtime_locked(context: &RuntimeContext) -> Result<RuntimeStatusReport> {
        let classified = classify_runtime(context)?;
        match classified.classification {
            RuntimeClassification::Owned | RuntimeClassification::Starting => {
                return report_from_classification(context, classified);
            }
            RuntimeClassification::ForeignListener => {
                return Err(runtime_conflict(
                    "the requested loopback port is owned by a foreign listener",
                ));
            }
            RuntimeClassification::Stopping => {
                return Err(runtime_conflict(
                    "the exact owned runtime is still stopping",
                ));
            }
            RuntimeClassification::Stale => cleanup_stale_runtime(context, &classified)?,
            RuntimeClassification::Stopped => {}
        }
        let request = LaunchRequest {
            schema_version: LAUNCH_REQUEST_SCHEMA.to_owned(),
            session_token: Uuid::new_v4().simple().to_string(),
            session_key: context.session_key.clone(),
            bundle_root: context.bundle_root.clone(),
            bundle_manifest_sha256: context.bundle.manifest_sha256.clone(),
            bundle_commit: context.bundle.product_commit.clone(),
            workspace: stable_workspace_identity(&context.workspace),
            listen_address: context.listen_address.to_string(),
            url: context.url(),
            runtime_root: context.runtime_root.clone(),
            session_directory: context.session_directory.clone(),
            workbench_sha256: context.bundle.workbench_sha256.clone(),
            created_at: Utc::now().to_rfc3339(),
        };
        let owner_lock = lock_runtime_owner(context)?;
        write_new_canonical_json(&context.launch_request_path(), &request)?;
        let mut supervisor = match spawn_supervisor(context, &owner_lock) {
            Ok(supervisor) => supervisor,
            Err(error) => {
                cleanup_runtime_control_files(&context.session_directory)?;
                return Err(error);
            }
        };
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if child_exited_unreaped(&supervisor, &context.current_executable)? {
                cleanup_failed_start(context, &mut supervisor)?;
                return Err(runtime_unavailable(format!(
                    "runtime supervisor exited before readiness; see {}",
                    context.supervisor_log_path().display()
                )));
            }
            match classify_runtime(context) {
                Ok(classified) if classified.classification == RuntimeClassification::Owned => {
                    let after = verify_bundle_root(&context.bundle_root)?;
                    if after.manifest_sha256 != context.bundle.manifest_sha256 {
                        cleanup_failed_start(context, &mut supervisor)?;
                        return Err(runtime_unavailable(
                            "bundle identity changed while starting",
                        ));
                    }
                    let report = match report_from_classification(context, classified) {
                        Ok(report) => report,
                        Err(error) => {
                            cleanup_failed_start(context, &mut supervisor)?;
                            return Err(error);
                        }
                    };
                    drop(supervisor);
                    drop(owner_lock);
                    return Ok(report);
                }
                Ok(classified)
                    if classified.classification == RuntimeClassification::ForeignListener =>
                {
                    cleanup_failed_start(context, &mut supervisor)?;
                    return Err(runtime_conflict(
                        "a listener race prevented the exact owned runtime from starting",
                    ));
                }
                Ok(_) | Err(_) if Instant::now() < deadline => sleep(POLL_INTERVAL).await,
                Ok(_) | Err(_) => {
                    cleanup_failed_start(context, &mut supervisor)?;
                    return Err(runtime_unavailable(format!(
                        "runtime did not become ready within {} seconds; see {}",
                        START_TIMEOUT.as_secs(),
                        context.supervisor_log_path().display()
                    )));
                }
            }
        }
    }

    /// Verify and classify one exact runtime without changing workspace state.
    ///
    /// # Errors
    ///
    /// Returns an error when bundle, workspace, or recorded runtime authority
    /// cannot be independently verified.
    pub fn status_runtime(options: &RuntimeCommandOptions) -> Result<RuntimeStatusReport> {
        let context = resolve_context(options)?;
        ensure_runtime_directories(&context)?;
        let _lock = lock_command(&context)?;
        let classified = classify_runtime(&context)?;
        report_from_classification(&context, classified)
    }

    /// Stop only an authenticated owned runtime.
    ///
    /// # Errors
    ///
    /// Returns an error when the runtime is foreign, its ownership cannot be
    /// authenticated, or bounded shutdown and cleanup cannot be proven.
    pub async fn stop_runtime(options: &RuntimeCommandOptions) -> Result<RuntimeStatusReport> {
        let context = resolve_context(options)?;
        ensure_runtime_directories(&context)?;
        let _lock = lock_command(&context)?;
        stop_runtime_locked(&context).await
    }

    async fn stop_runtime_locked(context: &RuntimeContext) -> Result<RuntimeStatusReport> {
        let classified = classify_runtime(context)?;
        if let (Some(request), Some(authenticated)) =
            (classified.request.as_ref(), classified.response.as_ref())
            && runtime_owner_is_active(context)?
        {
            let response = if authenticated.state == SupervisorState::Stopping {
                authenticated.clone()
            } else {
                let response = authenticated_control(context, request, ControlOperation::Stop)?;
                validate_control_response(context, request, &response)?;
                if response.state != SupervisorState::Stopping {
                    return Err(runtime_unavailable(
                        "owned supervisor did not accept the stop request",
                    ));
                }
                response
            };
            wait_for_owned_stop(context, &response).await?;
        } else {
            match classified.classification {
                RuntimeClassification::Stale => cleanup_stale_runtime(context, &classified)?,
                RuntimeClassification::Stopped => {}
                RuntimeClassification::ForeignListener => {
                    return Err(runtime_conflict(
                        "the requested port has a foreign listener; nothing was signalled",
                    ));
                }
                RuntimeClassification::Owned
                | RuntimeClassification::Starting
                | RuntimeClassification::Stopping => {
                    return Err(runtime_conflict(
                        "the runtime owner is changing state without an authenticated control identity",
                    ));
                }
            }
        }
        let bundle = verify_bundle_root(&context.bundle_root)?;
        let refreshed = RuntimeContext {
            bundle,
            ..context.clone()
        };
        let final_classification = classify_runtime(&refreshed)?;
        if final_classification.classification == RuntimeClassification::ForeignListener {
            return Err(runtime_conflict(
                "the owned runtime stopped, but a foreign listener now occupies the port",
            ));
        }
        report_from_classification(&refreshed, final_classification)
    }

    /// Stop an exact owned runtime if necessary, then start from a fresh
    /// independent bundle verification.
    ///
    /// # Errors
    ///
    /// Returns an error when ownership or bundle authority cannot be verified,
    /// restart fails, or workspace/application identity changes across restart.
    pub async fn restart_runtime(options: &RuntimeCommandOptions) -> Result<RuntimeStatusReport> {
        let context = resolve_context(options)?;
        ensure_runtime_directories(&context)?;
        let _lock = lock_command(&context)?;
        let before_workspace = context.workspace.clone();
        let classified = classify_runtime(&context)?;
        if classified.classification == RuntimeClassification::ForeignListener {
            return Err(runtime_conflict(
                "restart cannot replace or signal a foreign listener",
            ));
        }
        let before_application_identity =
            if classified.classification == RuntimeClassification::Owned {
                Some(
                    classified
                        .response
                        .as_ref()
                        .and_then(|response| response.application_identity_sha256.clone())
                        .ok_or_else(|| {
                            runtime_unavailable(
                                "owned runtime has no authenticated application identity",
                            )
                        })?,
                )
            } else {
                None
            };
        if matches!(
            classified.classification,
            RuntimeClassification::Owned
                | RuntimeClassification::Starting
                | RuntimeClassification::Stopping
                | RuntimeClassification::Stale
        ) {
            let _ = stop_runtime_locked(&context).await?;
        }
        let refreshed = resolve_context(options)?;
        let report = start_runtime_locked(&refreshed).await?;
        if report.workspace != before_workspace {
            let _ = stop_runtime_locked(&refreshed).await;
            return Err(runtime_unavailable(
                "workspace identity changed across restart",
            ));
        }
        if let Some(before) = before_application_identity {
            let after = report.application_identity_sha256.as_ref().ok_or_else(|| {
                runtime_unavailable("restarted runtime did not expose its application identity")
            })?;
            if after != &before {
                let _ = stop_runtime_locked(&refreshed).await;
                return Err(runtime_unavailable(
                    "application authority identity changed across restart",
                ));
            }
        }
        Ok(report)
    }

    /// Run the exact bundled integrity CLI while no listener owns this runtime
    /// port.
    ///
    /// # Errors
    ///
    /// Returns an error when bundle/runtime/workspace authority is invalid,
    /// the port is occupied, or the bounded integrity replay is not successful.
    #[allow(clippy::too_many_lines)]
    pub fn integrity_runtime(options: &RuntimeCommandOptions) -> Result<RuntimeIntegrityReport> {
        let context = resolve_context(options)?;
        ensure_runtime_directories(&context)?;
        let _lock = lock_command(&context)?;
        let classified = classify_runtime(&context)?;
        if matches!(
            classified.classification,
            RuntimeClassification::Owned
                | RuntimeClassification::Starting
                | RuntimeClassification::Stopping
                | RuntimeClassification::ForeignListener
        ) {
            return Err(runtime_conflict(
                "integrity requires the selected runtime port to be stopped",
            ));
        }
        let cli = context.bundle_root.join("bin/migration-stream");
        let session_temp_directory = context.session_directory.join(SESSION_TEMP_DIRECTORY_NAME);
        ensure_secure_directory(&session_temp_directory)?;
        let mut command = Command::new(&cli);
        configure_runtime_temp_environment(&mut command, &session_temp_directory);
        command
            .arg("integrity")
            .arg("--workspace")
            .arg(&context.workspace.canonical_path)
            .current_dir(
                context
                    .runtime_root
                    .parent()
                    .unwrap_or(Path::new("/private/tmp")),
            );
        let bounded = run_command_bounded(&mut command, &cli, INTEGRITY_TIMEOUT);
        let cleanup = cleanup_runtime_temp_directory(&context.session_directory);
        let (status, stdout, stderr) = match bounded {
            Ok(result) => {
                cleanup?;
                result
            }
            Err(error) => {
                cleanup?;
                return Err(error);
            }
        };
        let after = verify_bundle_root(&context.bundle_root)?;
        if after.manifest_sha256 != context.bundle.manifest_sha256 {
            return Err(runtime_unavailable(
                "bundle identity changed during integrity replay",
            ));
        }
        if !status.success() {
            return Err(runtime_unavailable(format!(
                "bundled integrity CLI failed with {} and {} bounded diagnostic bytes",
                status,
                stderr.len()
            )));
        }
        let value = parse_unique_json(&stdout).map_err(|error| {
            runtime_unavailable(format!("integrity output is invalid: {error}"))
        })?;
        if value
            .get("schema_version")
            .and_then(serde_json::Value::as_str)
            != Some("migration-stream/cli/v1")
            || value.get("ok").and_then(serde_json::Value::as_bool) != Some(true)
        {
            return Err(runtime_unavailable(
                "integrity output has an unsupported CLI envelope",
            ));
        }
        let integrity_result = value
            .get("result")
            .cloned()
            .ok_or_else(|| runtime_unavailable("integrity result is absent"))?;
        if integrity_result
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
            || integrity_result
                .get("workspace")
                .and_then(serde_json::Value::as_str)
                != Some(context.workspace.canonical_path.to_string_lossy().as_ref())
        {
            return Err(runtime_unavailable(
                "integrity result does not prove the selected workspace",
            ));
        }
        let (_, filesystem_device, filesystem_inode) =
            capture_workspace_filesystem_identity(&context.workspace.canonical_path)?;
        let workspace_after = capture_workspace_authority(
            context.workspace.canonical_path.clone(),
            filesystem_device,
            filesystem_inode,
        )?;
        if workspace_after != context.workspace {
            return Err(runtime_unavailable(
                "workspace identity changed during integrity replay",
            ));
        }
        Ok(RuntimeIntegrityReport {
            schema_version: RUNTIME_INTEGRITY_SCHEMA,
            bundle: context.bundle,
            workspace: context.workspace,
            bundled_cli_path: cli,
            integrity_result,
        })
    }

    fn run_command_bounded(
        command: &mut Command,
        executable: &Path,
        duration: Duration,
    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>)> {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| KernelError::io(executable, error))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| runtime_unavailable("bounded command stdout is unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| runtime_unavailable("bounded command stderr is unavailable"))?;
        let stdout_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout
                .take(MAXIMUM_INTEGRITY_OUTPUT_BYTES + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let stderr_reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr
                .take(MAXIMUM_INTEGRITY_OUTPUT_BYTES + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let deadline = Instant::now() + duration;
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| KernelError::io(executable, error))?
            {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(runtime_unavailable(
                    "bundled integrity CLI exceeded its time bound",
                ));
            }
            std::thread::sleep(POLL_INTERVAL);
        };
        let stdout = stdout_reader
            .join()
            .map_err(|_| runtime_unavailable("integrity stdout reader failed"))?
            .map_err(|error| KernelError::io(executable, error))?;
        let stderr = stderr_reader
            .join()
            .map_err(|_| runtime_unavailable("integrity stderr reader failed"))?
            .map_err(|error| KernelError::io(executable, error))?;
        if u64::try_from(stdout.len()).unwrap_or(u64::MAX) > MAXIMUM_INTEGRITY_OUTPUT_BYTES
            || u64::try_from(stderr.len()).unwrap_or(u64::MAX) > MAXIMUM_INTEGRITY_OUTPUT_BYTES
        {
            return Err(runtime_unavailable(
                "bundled integrity CLI output exceeds its byte bound",
            ));
        }
        Ok((status, stdout, stderr))
    }

    /// Execute the hidden session-local supervisor.
    ///
    /// # Errors
    ///
    /// Returns an error when the inherited session authority, bundle, child
    /// process, listener, or bounded lifecycle cleanup cannot be proven.
    #[allow(clippy::too_many_lines)]
    pub async fn run_supervisor(request_path: &Path) -> Result<()> {
        let current_executable = current_bundle_executable()?;
        let bundle_root = bundle_root_from_executable(&current_executable)?;
        let bundle = verify_bundle_root(&bundle_root)?;
        let request: LaunchRequest = read_canonical_json(request_path)?;
        validate_launch_request(&request, request_path, &bundle_root, &bundle)?;
        validate_secure_runtime_directory(&request.runtime_root)?;
        validate_secure_runtime_directory(&request.session_directory)?;
        validate_inherited_owner_lock(&request.session_directory)?;
        let session_temp_directory = request.session_directory.join(SESSION_TEMP_DIRECTORY_NAME);
        ensure_secure_directory(&session_temp_directory)?;
        let control_path = request.session_directory.join(CONTROL_SOCKET_FILENAME);
        let readiness_path = request.session_directory.join(READINESS_SOCKET_FILENAME);
        if control_path.as_os_str().as_encoded_bytes().len() > MAXIMUM_UNIX_SOCKET_PATH_BYTES {
            return Err(runtime_unavailable(
                "runtime control socket path is too long",
            ));
        }
        if fs::symlink_metadata(&control_path).is_ok() {
            return Err(runtime_conflict("runtime control socket already exists"));
        }
        if readiness_path.as_os_str().as_encoded_bytes().len() > MAXIMUM_UNIX_SOCKET_PATH_BYTES
            || fs::symlink_metadata(&readiness_path).is_ok()
        {
            return Err(runtime_conflict(
                "runtime readiness socket path is unavailable",
            ));
        }
        let listener = UnixListener::bind(&control_path)
            .map_err(|error| KernelError::io(&control_path, error))?;
        let _control_guard = SocketPathGuard(control_path.clone());
        set_exact_mode(&control_path, 0o600)?;
        verify_owned_socket(&control_path)?;
        let readiness_listener = UnixListener::bind(&readiness_path)
            .map_err(|error| KernelError::io(&readiness_path, error))?;
        let _readiness_guard = SocketPathGuard(readiness_path.clone());
        set_exact_mode(&readiness_path, 0o600)?;
        verify_owned_socket(&readiness_path)?;

        let workbench_path = bundle_root.join("bin/migration-stream-workbench");
        let workbench_log_path = request.session_directory.join(WORKBENCH_LOG_FILENAME);
        let log = open_private_log(&workbench_log_path, true)?;
        let stderr = log
            .try_clone()
            .map_err(|error| KernelError::io(&workbench_log_path, error))?;
        let mut command = Command::new(&workbench_path);
        configure_runtime_temp_environment(&mut command, &session_temp_directory);
        command
            .arg("--workspace")
            .arg(&request.workspace.canonical_path)
            .arg("--engagement-setup")
            .arg("--engagement-intake-policy")
            .arg(bundle_root.join("config/engagement-intake-policy.json"))
            .arg("--engagement-profiling-policy")
            .arg(bundle_root.join("config/engagement-profiling-policy.json"))
            .arg("--engagement-validation-policy")
            .arg(bundle_root.join("config/engagement-validation-policy.json"))
            .arg("--object-packs")
            .arg(bundle_root.join("object-packs"))
            .arg("--profile-schema")
            .arg(bundle_root.join("schemas/migration-object-profile.schema.json"))
            .arg("--mapping-schema")
            .arg(bundle_root.join("schemas/source-mapping-version.schema.json"))
            .arg("--assets")
            .arg(bundle_root.join("web"))
            .arg("--listen")
            .arg(&request.listen_address)
            .arg("--parent-watchdog")
            .arg("--readiness-socket")
            .arg(&readiness_path)
            .arg("--runtime-session-token")
            .arg(&request.session_token)
            .current_dir(&request.session_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr));
        let mut child = command
            .spawn()
            .map_err(|error| KernelError::io(&workbench_path, error))?;
        let child_pid = child.id();
        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| runtime_unavailable("workbench watchdog pipe is unavailable"))?;
        let supervisor_pid = std::process::id();
        let mut record = SessionRecord {
            schema_version: SESSION_RECORD_SCHEMA.to_owned(),
            session_token_sha256: token_hash(&request.session_token),
            session_key: request.session_key.clone(),
            bundle_root: request.bundle_root.clone(),
            bundle_manifest_sha256: request.bundle_manifest_sha256.clone(),
            bundle_commit: request.bundle_commit.clone(),
            workspace: request.workspace.clone(),
            listen_address: request.listen_address.clone(),
            url: request.url.clone(),
            supervisor_pid,
            workbench_pid: child_pid,
            // The launcher creates the supervisor as the leader of a fresh
            // process group. The workbench inherits that group so an
            // unsuccessful startup can be rolled back through the launcher's
            // still-live supervisor Child. After readiness, only this
            // supervisor signals and reaps its exact workbench Child.
            workbench_process_group_id: supervisor_pid,
            workbench_sha256: request.workbench_sha256.clone(),
            log_path: workbench_log_path,
            state: SupervisorState::Starting,
            started_at: Utc::now().to_rfc3339(),
            application_identity_sha256: None,
            failure: None,
        };
        write_replace_canonical_json(
            &request.session_directory.join(SESSION_RECORD_FILENAME),
            &record,
        )?;

        let result = supervise_child(
            &listener,
            &control_path,
            &readiness_listener,
            &readiness_path,
            &request,
            &mut record,
            &mut child,
            child_stdin,
        )
        .await;
        if result.is_ok() {
            cleanup_runtime_control_files(&request.session_directory)?;
        } else {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = terminate_owned_child(&mut child, None).await;
            }
            let _ = fs::remove_file(&control_path);
            let _ = fs::remove_file(&readiness_path);
        }
        result
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn supervise_child(
        listener: &UnixListener,
        _control_path: &Path,
        readiness_listener: &UnixListener,
        readiness_path: &Path,
        request: &LaunchRequest,
        record: &mut SessionRecord,
        child: &mut Child,
        child_stdin: ChildStdin,
    ) -> Result<()> {
        let mut watchdog = Some(child_stdin);
        let mut readiness_verified = false;
        let mut ticker = interval(POLL_INTERVAL);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(|error| runtime_unavailable(format!("TERM handler failed: {error}")))?;
        let mut interrupt =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|error| runtime_unavailable(format!("INT handler failed: {error}")))?;

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if let Some(status) = child.try_wait().map_err(|error| {
                        KernelError::io(&request.session_directory, error)
                    })? {
                        record.state = SupervisorState::Failed;
                        record.failure = Some(format!("workbench exited with {status}"));
                        write_replace_canonical_json(
                            &request.session_directory.join(SESSION_RECORD_FILENAME),
                            record,
                        )?;
                        drop(watchdog.take());
                        return Err(runtime_unavailable(
                            record.failure.clone().unwrap_or_else(|| "workbench exited".to_owned()),
                        ));
                    }
                    if record.state == SupervisorState::Starting && readiness_verified {
                        let after = verify_bundle_root(&request.bundle_root)?;
                        if after.manifest_sha256 != request.bundle_manifest_sha256 {
                            record.state = SupervisorState::Failed;
                            record.failure = Some("bundle changed during startup".to_owned());
                            write_replace_canonical_json(
                                &request.session_directory.join(SESSION_RECORD_FILENAME),
                                record,
                            )?;
                            terminate_owned_child(child, watchdog.take()).await?;
                            return Err(runtime_unavailable("bundle changed during startup"));
                        }
                        if let Ok(application_identity_sha256) =
                            probe_runtime_identity(request, record.workbench_pid)
                        {
                            record.application_identity_sha256 =
                                Some(application_identity_sha256);
                            record.state = SupervisorState::Ready;
                            write_replace_canonical_json(
                                &request.session_directory.join(SESSION_RECORD_FILENAME),
                                record,
                            )?;
                            let _ = fs::remove_file(readiness_path);
                        }
                    }
                }
                accepted = listener.accept() => {
                    let Ok((mut stream, _)) = accepted else {
                        continue;
                    };
                    let Ok(control) = read_control_request(&mut stream).await else {
                        continue;
                    };
                    if control.schema_version != CONTROL_REQUEST_SCHEMA
                        || control.session_token != request.session_token
                    {
                        let response = control_response(record, false, Some(
                            "control request did not authenticate this session".to_owned(),
                        ));
                        let _ = write_control_response(&mut stream, &response).await;
                        continue;
                    }
                    match control.operation {
                        ControlOperation::Status => {
                            let response = control_response(record, true, None);
                            let _ = write_control_response(&mut stream, &response).await;
                        }
                        ControlOperation::Stop => {
                            record.state = SupervisorState::Stopping;
                            write_replace_canonical_json(
                                &request.session_directory.join(SESSION_RECORD_FILENAME),
                                record,
                            )?;
                            let response = control_response(record, true, None);
                            let _ = write_control_response(&mut stream, &response).await;
                            terminate_owned_child(child, watchdog.take()).await?;
                            cleanup_runtime_control_files(&request.session_directory)?;
                            return Ok(());
                        }
                    }
                }
                accepted = readiness_listener.accept(), if record.state == SupervisorState::Starting => {
                    let Ok((mut stream, _)) = accepted else {
                        continue;
                    };
                    let Ok(readiness) = read_runtime_readiness(&mut stream).await else {
                        continue;
                    };
                    if !runtime_readiness_matches(
                        &readiness,
                        request,
                        record.workbench_pid,
                    ) {
                        continue;
                    }
                    readiness_verified = true;
                }
                _ = terminate.recv() => {
                    record.state = SupervisorState::Stopping;
                    let _ = write_replace_canonical_json(
                        &request.session_directory.join(SESSION_RECORD_FILENAME),
                        record,
                    );
                    terminate_owned_child(child, watchdog.take()).await?;
                    return Ok(());
                }
                _ = interrupt.recv() => {
                    record.state = SupervisorState::Stopping;
                    let _ = write_replace_canonical_json(
                        &request.session_directory.join(SESSION_RECORD_FILENAME),
                        record,
                    );
                    terminate_owned_child(child, watchdog.take()).await?;
                    return Ok(());
                }
            }
        }
    }

    async fn read_control_request(stream: &mut tokio::net::UnixStream) -> Result<ControlRequest> {
        let mut bytes = Vec::new();
        timeout(
            CONTROL_TIMEOUT,
            stream
                .take(MAXIMUM_RUNTIME_JSON_BYTES + 1)
                .read_to_end(&mut bytes),
        )
        .await
        .map_err(|_| runtime_unavailable("control request timed out"))?
        .map_err(|error| runtime_unavailable(format!("control request read failed: {error}")))?;
        if bytes.is_empty()
            || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAXIMUM_RUNTIME_JSON_BYTES
        {
            return Err(runtime_unavailable(
                "control request exceeds its byte bound",
            ));
        }
        parse_canonical_json(&bytes, "control request")
    }

    async fn read_runtime_readiness(
        stream: &mut tokio::net::UnixStream,
    ) -> Result<RuntimeReadiness> {
        let mut bytes = Vec::new();
        timeout(
            CONTROL_TIMEOUT,
            stream
                .take(MAXIMUM_RUNTIME_JSON_BYTES + 1)
                .read_to_end(&mut bytes),
        )
        .await
        .map_err(|_| runtime_unavailable("runtime readiness timed out"))?
        .map_err(|error| runtime_unavailable(format!("runtime readiness read failed: {error}")))?;
        if bytes.is_empty()
            || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAXIMUM_RUNTIME_JSON_BYTES
        {
            return Err(runtime_unavailable(
                "runtime readiness exceeds its byte bound",
            ));
        }
        parse_canonical_json(&bytes, "runtime readiness")
    }

    fn runtime_readiness_matches(
        readiness: &RuntimeReadiness,
        request: &LaunchRequest,
        expected_process_id: u32,
    ) -> bool {
        readiness.schema_version == "migration-stream/assisted-local-runtime/readiness/v2"
            && readiness.session_token == request.session_token
            && readiness.process_id == expected_process_id
            && readiness.listen_address == request.listen_address
            && readiness.workspace_path == request.workspace.canonical_path
            && readiness.filesystem_identity_sha256 == request.workspace.filesystem_identity_sha256
    }

    async fn write_control_response(
        stream: &mut tokio::net::UnixStream,
        response: &ControlResponse,
    ) -> Result<()> {
        let bytes = canonical_json_bytes(&serde_json::to_value(response)?)?;
        timeout(CONTROL_TIMEOUT, stream.write_all(&bytes))
            .await
            .map_err(|_| runtime_unavailable("control response timed out"))?
            .map_err(|error| {
                runtime_unavailable(format!("control response write failed: {error}"))
            })?;
        timeout(CONTROL_TIMEOUT, stream.shutdown())
            .await
            .map_err(|_| runtime_unavailable("control response shutdown timed out"))?
            .map_err(|error| {
                runtime_unavailable(format!("control response shutdown failed: {error}"))
            })
    }

    fn control_response(
        record: &SessionRecord,
        accepted: bool,
        reason: Option<String>,
    ) -> ControlResponse {
        ControlResponse {
            schema_version: CONTROL_RESPONSE_SCHEMA.to_owned(),
            accepted,
            state: record.state,
            session_key: record.session_key.clone(),
            bundle_root: record.bundle_root.clone(),
            bundle_manifest_sha256: record.bundle_manifest_sha256.clone(),
            bundle_commit: record.bundle_commit.clone(),
            workspace: record.workspace.clone(),
            listen_address: record.listen_address.clone(),
            url: record.url.clone(),
            supervisor_pid: record.supervisor_pid,
            workbench_pid: record.workbench_pid,
            workbench_process_group_id: record.workbench_process_group_id,
            workbench_sha256: record.workbench_sha256.clone(),
            log_path: record.log_path.clone(),
            application_identity_sha256: record.application_identity_sha256.clone(),
            reason,
        }
    }

    async fn terminate_owned_child(
        child: &mut Child,
        watchdog: Option<ChildStdin>,
    ) -> Result<ExitStatus> {
        terminate_owned_child_bounded(child, watchdog, TERM_TIMEOUT, KILL_TIMEOUT).await
    }

    async fn terminate_owned_child_bounded(
        child: &mut Child,
        watchdog: Option<ChildStdin>,
        term_timeout: Duration,
        kill_timeout: Duration,
    ) -> Result<ExitStatus> {
        let process = pid_from_u32(child.id())?;
        drop(watchdog);
        // This Child is retained by the authenticated supervisor and is not
        // reaped until the final wait below, so its PID cannot be reused while
        // either signal is issued. The shared process group is reserved for
        // startup rollback by the launcher that still owns the supervisor
        // Child; a post-start launcher never signals it.
        let _ = kill_process(process, Signal::TERM);
        if let Some(status) = wait_child_bounded(child, term_timeout).await? {
            return Ok(status);
        }
        let _ = kill_process(process, Signal::KILL);
        wait_child_bounded(child, kill_timeout)
            .await?
            .ok_or_else(|| runtime_unavailable("owned workbench could not be reaped"))
    }

    async fn wait_child_bounded(
        child: &mut Child,
        duration: Duration,
    ) -> Result<Option<ExitStatus>> {
        let deadline = Instant::now() + duration;
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| runtime_unavailable(format!("child wait failed: {error}")))?
            {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            sleep(POLL_INTERVAL).await;
        }
    }

    fn resolve_context(options: &RuntimeCommandOptions) -> Result<RuntimeContext> {
        if options.port == 0 {
            return Err(runtime_conflict(
                "assisted runtime port must be explicit and non-zero",
            ));
        }
        let current_executable = current_bundle_executable()?;
        let bundle_root = bundle_root_from_executable(&current_executable)?;
        let bundle = verify_bundle_root(&bundle_root)?;
        let (workspace_path, filesystem_device, filesystem_inode) =
            capture_workspace_filesystem_identity(&options.workspace)?;
        let workspace =
            capture_workspace_authority(workspace_path, filesystem_device, filesystem_inode)?;
        if paths_overlap(&bundle_root, &workspace.canonical_path) {
            return Err(runtime_conflict(
                "workspace and immutable bundle directories must not overlap",
            ));
        }
        let runtime_root = default_runtime_root()?;
        if paths_overlap(&bundle_root, &runtime_root)
            || paths_overlap(&workspace.canonical_path, &runtime_root)
        {
            return Err(runtime_conflict(
                "runtime metadata root must be outside bundle and workspace",
            ));
        }
        let listen_address = options.listen_address();
        let session_key = session_key(&workspace.filesystem_identity_sha256);
        let session_directory =
            runtime_root.join(format!("{SESSION_DIRECTORY_PREFIX}{}", &session_key[..24]));
        let socket = session_directory.join(CONTROL_SOCKET_FILENAME);
        if socket.as_os_str().as_encoded_bytes().len() > MAXIMUM_UNIX_SOCKET_PATH_BYTES {
            return Err(runtime_conflict(
                "runtime root is too long for the session control socket",
            ));
        }
        Ok(RuntimeContext {
            bundle,
            bundle_root,
            current_executable,
            workspace,
            listen_address,
            runtime_root,
            session_directory,
            session_key,
        })
    }

    fn current_bundle_executable() -> Result<PathBuf> {
        let current = std::env::current_exe()
            .map_err(|error| runtime_unavailable(format!("current executable: {error}")))?;
        let metadata =
            fs::symlink_metadata(&current).map_err(|error| KernelError::io(&current, error))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(runtime_unavailable(
                "launcher executable is not one real file",
            ));
        }
        current
            .canonicalize()
            .map_err(|error| KernelError::io(&current, error))
    }

    fn bundle_root_from_executable(executable: &Path) -> Result<PathBuf> {
        let libexec = executable
            .parent()
            .filter(|path| path.file_name().is_some_and(|name| name == "libexec"))
            .ok_or_else(|| {
                runtime_unavailable("runtime controller is not inside the bundle libexec directory")
            })?;
        let root = libexec
            .parent()
            .ok_or_else(|| runtime_unavailable("launcher has no bundle root"))?;
        let expected = root.join("libexec/migration-stream-local-runtime");
        let expected = expected
            .canonicalize()
            .map_err(|error| KernelError::io(&expected, error))?;
        if expected != executable {
            return Err(runtime_unavailable(
                "current executable is not the fixed bundle runtime controller",
            ));
        }
        root.canonicalize()
            .map_err(|error| KernelError::io(root, error))
    }

    fn capture_workspace_filesystem_identity(path: &Path) -> Result<(PathBuf, u64, u64)> {
        if !path.is_absolute() {
            return Err(runtime_conflict(
                "workspace must be an explicit absolute path",
            ));
        }
        let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(runtime_conflict(
                "workspace must be one existing real non-link directory",
            ));
        }
        let canonical = path
            .canonicalize()
            .map_err(|error| KernelError::io(path, error))?;
        let canonical_metadata =
            fs::symlink_metadata(&canonical).map_err(|error| KernelError::io(&canonical, error))?;
        if !canonical_metadata.is_dir()
            || canonical_metadata.file_type().is_symlink()
            || canonical_metadata.dev() != metadata.dev()
            || canonical_metadata.ino() != metadata.ino()
        {
            return Err(runtime_conflict(
                "workspace filesystem identity changed while resolving it",
            ));
        }
        Ok((canonical, metadata.dev(), metadata.ino()))
    }

    fn capture_workspace_authority(
        canonical: PathBuf,
        filesystem_device: u64,
        filesystem_inode: u64,
    ) -> Result<RuntimeWorkspaceIdentity> {
        let status = ObjectWorkspaceService::new(&canonical)?.inspect()?;
        Ok(RuntimeWorkspaceIdentity {
            canonical_path: canonical,
            filesystem_identity_sha256: super::unix_workspace_filesystem_identity_sha256(
                filesystem_device,
                filesystem_inode,
            ),
            object_workspace_state: status.state,
            object_workspace: status.workspace,
        })
    }

    fn default_runtime_root() -> Result<PathBuf> {
        let base = if cfg!(target_os = "linux") {
            secure_linux_runtime_base()?
        } else {
            PathBuf::from("/private/tmp")
        };
        Ok(base.join(format!("{RUNTIME_DIRECTORY_NAME}-{}", geteuid().as_raw())))
    }

    fn secure_linux_runtime_base() -> Result<PathBuf> {
        select_linux_runtime_base(
            std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
            Path::new("/tmp"),
        )
    }

    fn select_linux_runtime_base(candidate: Option<PathBuf>, fallback: &Path) -> Result<PathBuf> {
        if let Some(candidate) = candidate
            && let Ok(verified) = verify_owned_private_real_directory(&candidate)
        {
            return Ok(verified);
        }
        verify_shared_temp_base(fallback)
    }

    fn verify_owned_private_real_directory(path: &Path) -> Result<PathBuf> {
        let canonical = canonical_real_directory(path)?;
        let metadata =
            fs::symlink_metadata(&canonical).map_err(|error| KernelError::io(&canonical, error))?;
        if metadata.uid() != geteuid().as_raw() || metadata.mode() & 0o777 != 0o700 {
            return Err(runtime_unavailable(
                "XDG runtime directory must be mode 0700 and owned by this user",
            ));
        }
        Ok(canonical)
    }

    fn verify_shared_temp_base(path: &Path) -> Result<PathBuf> {
        let canonical = canonical_real_directory(path)?;
        let metadata =
            fs::symlink_metadata(&canonical).map_err(|error| KernelError::io(&canonical, error))?;
        if metadata.uid() != 0 || metadata.mode() & 0o7777 != 0o1777 {
            return Err(runtime_unavailable(
                "Linux fallback runtime base must be the real root-owned sticky /tmp directory",
            ));
        }
        Ok(canonical)
    }

    fn canonical_real_directory(path: &Path) -> Result<PathBuf> {
        if !path.is_absolute() {
            return Err(runtime_unavailable(
                "runtime base directory must be an absolute real directory",
            ));
        }
        let mut current = PathBuf::from("/");
        for component in path.components() {
            let std::path::Component::Normal(part) = component else {
                continue;
            };
            current.push(part);
            let metadata =
                fs::symlink_metadata(&current).map_err(|error| KernelError::io(&current, error))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(runtime_unavailable(format!(
                    "runtime base ancestor {} must be one real directory",
                    current.display()
                )));
            }
        }
        let initial = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
        let canonical = path
            .canonicalize()
            .map_err(|error| KernelError::io(path, error))?;
        let resolved =
            fs::symlink_metadata(&canonical).map_err(|error| KernelError::io(&canonical, error))?;
        if !resolved.is_dir()
            || resolved.file_type().is_symlink()
            || initial.dev() != resolved.dev()
            || initial.ino() != resolved.ino()
        {
            return Err(runtime_unavailable(
                "runtime base identity changed while resolving it",
            ));
        }
        Ok(canonical)
    }

    fn session_key(filesystem_identity_sha256: &str) -> String {
        let mut digest = Sha256::new();
        digest.update(b"migration-stream-assisted-runtime-session-key/v2\0");
        digest.update(filesystem_identity_sha256.as_bytes());
        format!("{:x}", digest.finalize())
    }

    fn paths_overlap(left: &Path, right: &Path) -> bool {
        left == right || left.starts_with(right) || right.starts_with(left)
    }

    fn ensure_runtime_directories(context: &RuntimeContext) -> Result<()> {
        ensure_secure_directory(&context.runtime_root)?;
        ensure_secure_directory(&context.session_directory)
    }

    fn ensure_secure_directory(path: &Path) -> Result<()> {
        match fs::symlink_metadata(path) {
            Ok(_) => validate_secure_runtime_directory(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::DirBuilder::new().mode(0o700).create(path) {
                    Ok(()) => {}
                    Err(create) if create.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(create) => return Err(KernelError::io(path, create)),
                }
                validate_secure_runtime_directory(path)
            }
            Err(error) => Err(KernelError::io(path, error)),
        }
    }

    fn validate_secure_runtime_directory(path: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != geteuid().as_raw()
            || metadata.mode() & 0o777 != 0o700
        {
            return Err(runtime_unavailable(format!(
                "runtime directory {} must be a mode-0700 real directory owned by this user",
                path.display()
            )));
        }
        Ok(())
    }

    fn lock_command(context: &RuntimeContext) -> Result<File> {
        let path = context.command_lock_path();
        let file = open_private_lock(&path)?;
        let deadline = Instant::now() + COMMAND_LOCK_TIMEOUT;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(file),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err(runtime_conflict(
                            "another lifecycle command still owns the bounded session lock",
                        ));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(error) => return Err(KernelError::io(&path, error)),
            }
        }
    }

    fn lock_runtime_owner(context: &RuntimeContext) -> Result<File> {
        let path = context.owner_lock_path();
        let file = open_private_lock(&path)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(file),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Err(runtime_conflict(
                "an exact runtime supervisor already owns this workspace session",
            )),
            Err(error) => Err(KernelError::io(&path, error)),
        }
    }

    fn runtime_owner_is_active(context: &RuntimeContext) -> Result<bool> {
        let path = context.owner_lock_path();
        let file = open_private_lock(&path)?;
        match file.try_lock_exclusive() {
            Ok(()) => {
                fs2::FileExt::unlock(&file).map_err(|error| KernelError::io(&path, error))?;
                Ok(false)
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
            Err(error) => Err(KernelError::io(&path, error)),
        }
    }

    fn validate_inherited_owner_lock(session_directory: &Path) -> Result<()> {
        let standard_input = std::io::stdin();
        validate_owner_lock_descriptor(session_directory, &standard_input)
    }

    fn validate_owner_lock_descriptor(
        session_directory: &Path,
        inherited: &impl std::os::fd::AsFd,
    ) -> Result<()> {
        let path = session_directory.join(OWNER_LOCK_FILENAME);
        let named = fs::symlink_metadata(&path).map_err(|error| KernelError::io(&path, error))?;
        let opened = rustix::fs::fstat(inherited.as_fd())
            .map_err(|error| KernelError::io(&path, std::io::Error::from(error)))?;
        if !named.is_file()
            || named.file_type().is_symlink()
            || named.uid() != geteuid().as_raw()
            || named.mode() & 0o777 != 0o600
            || named.nlink() != 1
            || normalized_device_id(opened.st_dev) != Some(named.dev())
            || opened.st_ino != named.ino()
        {
            return Err(runtime_unavailable(
                "runtime supervisor did not inherit the exact owner lock",
            ));
        }
        let observer = open_private_lock(&path)?;
        match observer.try_lock_exclusive() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
            Ok(()) => {
                fs2::FileExt::unlock(&observer).map_err(|error| KernelError::io(&path, error))?;
                Err(runtime_unavailable(
                    "runtime supervisor owner lock is not held",
                ))
            }
            Err(error) => Err(KernelError::io(&path, error)),
        }
    }

    fn normalized_device_id<T>(device: T) -> Option<u64>
    where
        T: TryInto<u64>,
    {
        device.try_into().ok()
    }

    fn open_private_lock(path: &Path) -> Result<File> {
        let descriptor = open(
            path,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| KernelError::io(path, std::io::Error::from(error)))?;
        let file = File::from(descriptor);
        let opened = file
            .metadata()
            .map_err(|error| KernelError::io(path, error))?;
        let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
        if !opened.is_file()
            || !named.is_file()
            || named.file_type().is_symlink()
            || opened.dev() != named.dev()
            || opened.ino() != named.ino()
            || named.uid() != geteuid().as_raw()
            || named.mode() & 0o777 != 0o600
            || named.nlink() != 1
        {
            return Err(runtime_unavailable(
                "runtime command lock has an unsafe identity",
            ));
        }
        Ok(file)
    }

    #[allow(clippy::too_many_lines)]
    fn classify_runtime(context: &RuntimeContext) -> Result<ClassifiedRuntime> {
        let listener_active = listener_is_active(context.listen_address);
        if !context.runtime_root.exists() || !context.session_directory.exists() {
            return Ok(ClassifiedRuntime {
                classification: if listener_active {
                    RuntimeClassification::ForeignListener
                } else {
                    RuntimeClassification::Stopped
                },
                request: None,
                response: None,
                reason: listener_active
                    .then(|| "a listener exists without an owned runtime session".to_owned()),
            });
        }
        validate_secure_runtime_directory(&context.runtime_root)?;
        validate_secure_runtime_directory(&context.session_directory)?;
        let owner_active = runtime_owner_is_active(context)?;
        let request_path = context.launch_request_path();
        let request: LaunchRequest = match fs::symlink_metadata(&request_path) {
            Ok(_) => read_canonical_json(&request_path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ClassifiedRuntime {
                    classification: if listener_active {
                        RuntimeClassification::ForeignListener
                    } else if owner_active {
                        RuntimeClassification::Starting
                    } else {
                        RuntimeClassification::Stopped
                    },
                    request: None,
                    response: None,
                    reason: if listener_active {
                        Some("listener has no owned launch request".to_owned())
                    } else if owner_active {
                        Some("runtime owner lock is completing its startup handoff".to_owned())
                    } else {
                        None
                    },
                });
            }
            Err(error) => return Err(KernelError::io(&request_path, error)),
        };
        validate_request_for_context(&request, context)?;
        match authenticated_control(context, &request, ControlOperation::Status) {
            Ok(mut response) => {
                validate_control_response(context, &request, &response)?;
                let runtime_identity = if response.state == SupervisorState::Ready
                    && owner_active
                    && listener_active
                {
                    probe_runtime_identity(&request, response.workbench_pid).ok()
                } else {
                    None
                };
                if let Some(identity) = runtime_identity.as_ref() {
                    response.application_identity_sha256 = Some(identity.clone());
                }
                let classification = match response.state {
                    SupervisorState::Starting if owner_active => RuntimeClassification::Starting,
                    SupervisorState::Ready
                        if owner_active && listener_active && runtime_identity.is_some() =>
                    {
                        RuntimeClassification::Owned
                    }
                    SupervisorState::Ready if owner_active && listener_active => {
                        RuntimeClassification::ForeignListener
                    }
                    SupervisorState::Stopping if owner_active => RuntimeClassification::Stopping,
                    SupervisorState::Starting
                    | SupervisorState::Ready
                    | SupervisorState::Stopping
                    | SupervisorState::Failed => RuntimeClassification::Stale,
                };
                Ok(ClassifiedRuntime {
                    classification,
                    request: Some(request),
                    response: Some(response),
                    reason: match classification {
                        RuntimeClassification::Stale => {
                            Some("owned session record is not backed by its listener".to_owned())
                        }
                        RuntimeClassification::ForeignListener => Some(
                            "the active listener did not prove the exact runtime identity"
                                .to_owned(),
                        ),
                        _ => None,
                    },
                })
            }
            Err(_) => Ok(ClassifiedRuntime {
                classification: if listener_active {
                    RuntimeClassification::ForeignListener
                } else if owner_active {
                    RuntimeClassification::Starting
                } else {
                    RuntimeClassification::Stale
                },
                request: Some(request),
                response: None,
                reason: Some(if listener_active {
                    "listener does not authenticate as the recorded owned session".to_owned()
                } else if owner_active {
                    "runtime owner lock is completing its authenticated startup handoff".to_owned()
                } else {
                    "recorded session has no authenticated supervisor or listener".to_owned()
                }),
            }),
        }
    }

    fn listener_is_active(address: SocketAddrV4) -> bool {
        TcpStream::connect_timeout(&SocketAddr::V4(address), LISTENER_PROBE_TIMEOUT).is_ok()
    }

    fn probe_runtime_identity(
        request: &LaunchRequest,
        expected_workbench_pid: u32,
    ) -> Result<String> {
        let listen_address = parse_listen(&request.listen_address)?;
        let mut stream =
            TcpStream::connect_timeout(&SocketAddr::V4(listen_address), CONTROL_TIMEOUT).map_err(
                |error| {
                    runtime_unavailable(format!("application identity connection failed: {error}"))
                },
            )?;
        stream
            .set_read_timeout(Some(CONTROL_TIMEOUT))
            .map_err(|error| {
                runtime_unavailable(format!("application identity timeout: {error}"))
            })?;
        stream
            .set_write_timeout(Some(CONTROL_TIMEOUT))
            .map_err(|error| {
                runtime_unavailable(format!("application identity timeout: {error}"))
            })?;
        let http_request = format!(
            "GET /__migration-stream/runtime-identity HTTP/1.1\r\nHost: {}\r\nX-Migration-Stream-Runtime-Token: {}\r\nConnection: close\r\nAccept: application/json\r\n\r\n",
            request.listen_address, request.session_token
        );
        stream.write_all(http_request.as_bytes()).map_err(|error| {
            runtime_unavailable(format!("application identity request failed: {error}"))
        })?;
        let mut response = Vec::new();
        (&mut stream)
            .take(MAXIMUM_RUNTIME_JSON_BYTES + 16 * 1024)
            .read_to_end(&mut response)
            .map_err(|error| {
                runtime_unavailable(format!("application identity response failed: {error}"))
            })?;
        if u64::try_from(response.len()).unwrap_or(u64::MAX)
            > MAXIMUM_RUNTIME_JSON_BYTES + 16 * 1024
        {
            return Err(runtime_unavailable(
                "application identity response exceeds its byte bound",
            ));
        }
        let header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
            .ok_or_else(|| runtime_unavailable("application identity response is malformed"))?;
        let headers = std::str::from_utf8(&response[..header_end])
            .map_err(|_| runtime_unavailable("application identity headers are not UTF-8"))?;
        if !headers.starts_with("HTTP/1.1 200 ") && !headers.starts_with("HTTP/1.0 200 ") {
            return Err(runtime_unavailable(
                "application identity endpoint did not return success",
            ));
        }
        let body = &response[header_end..];
        let value = parse_unique_json(body).map_err(|error| {
            runtime_unavailable(format!("application identity JSON is invalid: {error}"))
        })?;
        let identity: RuntimeHttpIdentity = serde_json::from_value(value).map_err(|error| {
            runtime_unavailable(format!("application identity contract is invalid: {error}"))
        })?;
        if !runtime_http_identity_matches(&identity, request, expected_workbench_pid) {
            return Err(runtime_unavailable(
                "application identity does not match the exact owned runtime",
            ));
        }
        Ok(identity.application_identity_sha256)
    }

    fn runtime_http_identity_matches(
        identity: &RuntimeHttpIdentity,
        request: &LaunchRequest,
        expected_workbench_pid: u32,
    ) -> bool {
        identity.schema_version == APPLICATION_IDENTITY_SCHEMA
            && identity.session_token_sha256 == token_hash(&request.session_token)
            && identity.process_id == expected_workbench_pid
            && identity.listen_address == request.listen_address
            && identity.workspace_path == request.workspace.canonical_path
            && identity.filesystem_identity_sha256 == request.workspace.filesystem_identity_sha256
            && is_lower_hex_sha256(&identity.application_identity_sha256)
    }

    fn authenticated_control(
        context: &RuntimeContext,
        request: &LaunchRequest,
        operation: ControlOperation,
    ) -> Result<ControlResponse> {
        let socket = context.control_socket_path();
        verify_owned_socket(&socket)?;
        let mut stream =
            UnixStream::connect(&socket).map_err(|error| KernelError::io(&socket, error))?;
        stream
            .set_read_timeout(Some(CONTROL_TIMEOUT))
            .map_err(|error| KernelError::io(&socket, error))?;
        stream
            .set_write_timeout(Some(CONTROL_TIMEOUT))
            .map_err(|error| KernelError::io(&socket, error))?;
        let command = ControlRequest {
            schema_version: CONTROL_REQUEST_SCHEMA.to_owned(),
            session_token: request.session_token.clone(),
            operation,
        };
        let bytes = canonical_json_bytes(&serde_json::to_value(command)?)?;
        stream
            .write_all(&bytes)
            .map_err(|error| KernelError::io(&socket, error))?;
        stream
            .shutdown(Shutdown::Write)
            .map_err(|error| KernelError::io(&socket, error))?;
        let mut response = Vec::new();
        (&mut stream)
            .take(MAXIMUM_RUNTIME_JSON_BYTES + 1)
            .read_to_end(&mut response)
            .map_err(|error| KernelError::io(&socket, error))?;
        if response.is_empty()
            || u64::try_from(response.len()).unwrap_or(u64::MAX) > MAXIMUM_RUNTIME_JSON_BYTES
        {
            return Err(runtime_unavailable(
                "runtime control response exceeds its bound",
            ));
        }
        parse_canonical_json(&response, "runtime control response")
    }

    fn validate_control_response(
        context: &RuntimeContext,
        request: &LaunchRequest,
        response: &ControlResponse,
    ) -> Result<()> {
        if response.schema_version != CONTROL_RESPONSE_SCHEMA
            || !response.accepted
            || response.session_key != context.session_key
            || response.bundle_root != context.bundle_root
            || response.bundle_manifest_sha256 != request.bundle_manifest_sha256
            || response.bundle_commit != request.bundle_commit
            || response.workspace != request.workspace
            || !stable_workspace_matches_runtime(&response.workspace, &context.workspace)
            || response.listen_address != context.listen_address.to_string()
            || response.url != context.url()
            || response.workbench_sha256 != request.workbench_sha256
            || response.supervisor_pid == 0
            || response.workbench_pid == 0
            || response.workbench_pid == response.supervisor_pid
            || response.workbench_process_group_id != response.supervisor_pid
            || (response.state == SupervisorState::Ready
                && response
                    .application_identity_sha256
                    .as_deref()
                    .is_none_or(|value| !is_lower_hex_sha256(value)))
        {
            return Err(runtime_unavailable(
                "runtime control response does not match the exact session",
            ));
        }
        Ok(())
    }

    fn report_from_classification(
        context: &RuntimeContext,
        classified: ClassifiedRuntime,
    ) -> Result<RuntimeStatusReport> {
        if let (Some(request), Some(response)) =
            (classified.request.as_ref(), classified.response.as_ref())
        {
            validate_control_response(context, request, response)?;
        }
        Ok(RuntimeStatusReport {
            schema_version: RUNTIME_STATUS_SCHEMA,
            classification: classified.classification,
            bundle: context.bundle.clone(),
            workspace: context.workspace.clone(),
            listen_address: context.listen_address.to_string(),
            url: context.url(),
            runtime_root: context.runtime_root.clone(),
            session_directory: context.session_directory.clone(),
            supervisor_pid: classified
                .response
                .as_ref()
                .map(|response| response.supervisor_pid),
            workbench_pid: classified
                .response
                .as_ref()
                .map(|response| response.workbench_pid),
            workbench_process_group_id: classified
                .response
                .as_ref()
                .map(|response| response.workbench_process_group_id),
            log_path: classified
                .response
                .as_ref()
                .map(|response| response.log_path.clone()),
            application_identity_sha256: (classified.classification
                == RuntimeClassification::Owned)
                .then(|| {
                    classified
                        .response
                        .as_ref()
                        .and_then(|response| response.application_identity_sha256.clone())
                        .ok_or_else(|| {
                            runtime_unavailable(
                                "owned runtime has no authenticated application identity",
                            )
                        })
                })
                .transpose()?,
            reason: classified.reason,
        })
    }

    fn spawn_supervisor(context: &RuntimeContext, owner_lock: &File) -> Result<Child> {
        let log_path = context.supervisor_log_path();
        let log = open_private_log(&log_path, true)?;
        let stderr = log
            .try_clone()
            .map_err(|error| KernelError::io(&log_path, error))?;
        let inherited_owner_lock = owner_lock
            .try_clone()
            .map_err(|error| KernelError::io(context.owner_lock_path(), error))?;
        let mut command = Command::new(&context.current_executable);
        command
            .env_clear()
            .arg("__supervise")
            .arg("--request")
            .arg(context.launch_request_path())
            .current_dir(&context.session_directory)
            // Standard input is the inherited open-file description that owns
            // the lifetime workspace lock. The supervisor never reads it; the
            // lock releases only after both launcher and supervisor close it.
            .stdin(Stdio::from(inherited_owner_lock))
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .process_group(0);
        command
            .spawn()
            .map_err(|error| KernelError::io(&context.current_executable, error))
    }

    fn cleanup_failed_start(context: &RuntimeContext, supervisor: &mut Child) -> Result<()> {
        let supervisor_group = pid_from_u32(supervisor.id())?;
        let mut reaped = false;

        // Once the supervisor has created its authenticated control socket it
        // owns the only workbench Child handle and therefore owns graceful
        // TERM-to-KILL-to-reap cleanup. Give it the full bounded cleanup window
        // before using the launcher's still-live supervisor Child capability.
        if !child_exited_unreaped(supervisor, &context.current_executable)?
            && let Ok(request) =
                read_canonical_json::<LaunchRequest>(&context.launch_request_path())
            && let Ok(stop) = authenticated_control(context, &request, ControlOperation::Stop)
            && stop.supervisor_pid == supervisor.id()
            && stop.state == SupervisorState::Stopping
            && validate_control_response(context, &request, &stop).is_ok()
        {
            reaped = wait_child_exit_blocking(
                supervisor,
                TERM_TIMEOUT + KILL_TIMEOUT + CONTROL_TIMEOUT,
                &context.current_executable,
            )?;
        }
        if !reaped {
            terminate_live_startup_group(
                supervisor,
                supervisor_group,
                KILL_TIMEOUT,
                &context.current_executable,
            )?;
        }
        observe_process_group_extinct(supervisor_group, KILL_TIMEOUT)?;
        cleanup_runtime_control_files(&context.session_directory)?;
        if !runtime_control_files_absent(&context.session_directory)? {
            return Err(runtime_unavailable(
                "failed-start cleanup left an owned runtime identity",
            ));
        }
        let quiet_deadline = Instant::now() + LISTENER_QUIET_WINDOW;
        while Instant::now() < quiet_deadline {
            if listener_is_active(context.listen_address) {
                return Err(runtime_conflict(
                    "failed-start cleanup preserved a listener that did not prove owned identity",
                ));
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        if listener_is_active(context.listen_address) {
            return Err(runtime_conflict(
                "failed-start cleanup preserved a listener that did not prove owned identity",
            ));
        }
        Ok(())
    }

    fn child_exited_unreaped(child: &Child, executable: &Path) -> Result<bool> {
        let process = pid_from_u32(child.id())?;
        waitid(
            WaitId::Pid(process),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .map(|status| status.is_some())
        .map_err(|error| {
            runtime_unavailable(format!(
                "owned child exit probe failed for {}: {error}",
                executable.display()
            ))
        })
    }

    fn wait_child_exit_blocking(
        child: &mut Child,
        duration: Duration,
        executable: &Path,
    ) -> Result<bool> {
        let deadline = Instant::now() + duration;
        loop {
            if child
                .try_wait()
                .map_err(|error| KernelError::io(executable, error))?
                .is_some()
            {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn terminate_live_startup_group(
        supervisor: &mut Child,
        expected_group: Pid,
        duration: Duration,
        executable: &Path,
    ) -> Result<()> {
        let supervisor_pid = pid_from_u32(supervisor.id())?;
        let observed_group = match getpgid(Some(supervisor_pid)) {
            Ok(group) => group,
            Err(error) => {
                if !child_exited_unreaped(supervisor, executable)? {
                    return Err(runtime_unavailable(format!(
                        "live startup supervisor process group probe failed: {error}"
                    )));
                }
                // Some hosts no longer expose a zombie's group through
                // getpgid. Its unreaped Child PID is nevertheless still
                // reserved and was created as this exact group leader.
                expected_group
            }
        };
        if supervisor_pid != expected_group || observed_group != expected_group {
            return Err(runtime_unavailable(
                "live startup supervisor no longer owns its exact process group",
            ));
        }
        // The unreaped Child keeps its PID unavailable for reuse across this
        // check-and-signal boundary. The workbench inherited this exact group,
        // so a failed startup can be closed without trusting recorded PIDs.
        let _ = kill_process_group(expected_group, Signal::KILL);
        if !wait_child_exit_blocking(supervisor, duration, executable)? {
            return Err(runtime_unavailable(
                "owned startup supervisor could not be killed and reaped",
            ));
        }
        Ok(())
    }

    fn observe_process_group_extinct(process_group: Pid, duration: Duration) -> Result<()> {
        let deadline = Instant::now() + duration;
        loop {
            if test_kill_process_group(process_group).is_err() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(runtime_unavailable(
                    "startup supervisor process group remained active; no unowned process was signalled",
                ));
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    async fn wait_for_owned_stop(
        context: &RuntimeContext,
        authenticated_response: &ControlResponse,
    ) -> Result<()> {
        wait_for_owned_stop_bounded(
            context,
            authenticated_response,
            TERM_TIMEOUT + KILL_TIMEOUT + CONTROL_TIMEOUT,
        )
        .await
    }

    async fn wait_for_owned_stop_bounded(
        context: &RuntimeContext,
        _authenticated_response: &ControlResponse,
        duration: Duration,
    ) -> Result<()> {
        let deadline = Instant::now() + duration;
        loop {
            if owned_stop_complete(context)? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
            sleep(POLL_INTERVAL).await;
        }
        Err(runtime_unavailable(
            "authenticated supervisor did not complete bounded workbench cleanup; no recorded process identity was signalled",
        ))
    }

    fn owned_stop_complete(context: &RuntimeContext) -> Result<bool> {
        if runtime_owner_is_active(context)? {
            return Ok(false);
        }
        if runtime_control_files_absent(&context.session_directory)? {
            return Ok(true);
        }
        if listener_is_active(context.listen_address) {
            return Ok(false);
        }
        cleanup_runtime_control_files(&context.session_directory)?;
        runtime_control_files_absent(&context.session_directory)
    }

    fn runtime_control_files_absent(session_directory: &Path) -> Result<bool> {
        for name in [
            CONTROL_SOCKET_FILENAME,
            READINESS_SOCKET_FILENAME,
            SESSION_RECORD_FILENAME,
            LAUNCH_REQUEST_FILENAME,
            SESSION_TEMP_DIRECTORY_NAME,
        ] {
            let path = session_directory.join(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(runtime_unavailable(format!(
                        "runtime control {} became a symbolic link",
                        path.display()
                    )));
                }
                Ok(_) => return Ok(false),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(KernelError::io(&path, error)),
            }
        }
        Ok(true)
    }

    fn pid_from_u32(raw: u32) -> Result<Pid> {
        let raw =
            i32::try_from(raw).map_err(|_| runtime_unavailable("process identity exceeds i32"))?;
        Pid::from_raw(raw).ok_or_else(|| runtime_unavailable("process identity is zero"))
    }

    fn cleanup_stale_runtime(
        context: &RuntimeContext,
        classified: &ClassifiedRuntime,
    ) -> Result<()> {
        if listener_is_active(context.listen_address) {
            return Err(runtime_conflict(
                "stale runtime metadata is accompanied by a foreign listener",
            ));
        }
        if runtime_owner_is_active(context)? {
            return Err(runtime_conflict(
                "runtime metadata cannot be cleaned while its supervisor owner lock is active",
            ));
        }
        if classified.request.is_none() {
            return Ok(());
        }
        cleanup_runtime_control_files(&context.session_directory)
    }

    fn cleanup_runtime_control_files(session_directory: &Path) -> Result<()> {
        for (name, kind) in [
            (CONTROL_SOCKET_FILENAME, RuntimeFileKind::Socket),
            (READINESS_SOCKET_FILENAME, RuntimeFileKind::Socket),
            (SESSION_RECORD_FILENAME, RuntimeFileKind::Regular),
            (LAUNCH_REQUEST_FILENAME, RuntimeFileKind::Regular),
        ] {
            let path = session_directory.join(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    let expected = match kind {
                        RuntimeFileKind::Socket => metadata.file_type().is_socket(),
                        RuntimeFileKind::Regular => metadata.is_file(),
                    };
                    if !expected || metadata.file_type().is_symlink() {
                        return Err(runtime_unavailable(format!(
                            "runtime control {} has an unsafe type",
                            path.display()
                        )));
                    }
                    fs::remove_file(&path).map_err(|error| KernelError::io(&path, error))?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(KernelError::io(&path, error)),
            }
        }
        cleanup_runtime_temp_directory(session_directory)
    }

    fn cleanup_runtime_temp_directory(session_directory: &Path) -> Result<()> {
        let path = session_directory.join(SESSION_TEMP_DIRECTORY_NAME);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(KernelError::io(&path, error)),
        };
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(runtime_unavailable(
                "runtime temporary directory has an unsafe identity",
            ));
        }
        let mut entries = 0_usize;
        cleanup_runtime_temp_contents(&path, 0, &mut entries)?;
        fs::remove_dir(&path).map_err(|error| KernelError::io(&path, error))
    }

    fn cleanup_runtime_temp_contents(
        directory: &Path,
        depth: usize,
        entries: &mut usize,
    ) -> Result<()> {
        if depth > MAXIMUM_RUNTIME_TEMP_DEPTH {
            return Err(runtime_unavailable(
                "runtime temporary directory exceeds its depth bound",
            ));
        }
        for entry in fs::read_dir(directory).map_err(|error| KernelError::io(directory, error))? {
            let entry = entry.map_err(|error| KernelError::io(directory, error))?;
            *entries = entries
                .checked_add(1)
                .ok_or_else(|| runtime_unavailable("runtime temporary entry count overflow"))?;
            if *entries > MAXIMUM_RUNTIME_TEMP_ENTRIES {
                return Err(runtime_unavailable(
                    "runtime temporary directory exceeds its entry bound",
                ));
            }
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|error| KernelError::io(&path, error))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                if metadata.uid() != geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
                    return Err(runtime_unavailable(
                        "runtime temporary child directory has unsafe permissions",
                    ));
                }
                cleanup_runtime_temp_contents(&path, depth + 1, entries)?;
                fs::remove_dir(&path).map_err(|error| KernelError::io(&path, error))?;
            } else {
                fs::remove_file(&path).map_err(|error| KernelError::io(&path, error))?;
            }
        }
        Ok(())
    }

    fn configure_runtime_temp_environment(command: &mut Command, temp_directory: &Path) {
        command
            .env_clear()
            .env("TMPDIR", temp_directory)
            .env("SQLITE_TMPDIR", temp_directory);
    }

    #[derive(Clone, Copy)]
    enum RuntimeFileKind {
        Socket,
        Regular,
    }

    fn validate_launch_request(
        request: &LaunchRequest,
        request_path: &Path,
        bundle_root: &Path,
        bundle: &BundleVerification,
    ) -> Result<()> {
        let (workspace_path, filesystem_device, filesystem_inode) =
            capture_workspace_filesystem_identity(&request.workspace.canonical_path)?;
        let context = RuntimeContext {
            bundle: bundle.clone(),
            bundle_root: bundle_root.to_path_buf(),
            current_executable: current_bundle_executable()?,
            workspace: capture_workspace_authority(
                workspace_path,
                filesystem_device,
                filesystem_inode,
            )?,
            listen_address: parse_listen(&request.listen_address)?,
            runtime_root: request.runtime_root.clone(),
            session_directory: request.session_directory.clone(),
            session_key: request.session_key.clone(),
        };
        validate_request_for_context(request, &context)?;
        if request_path != context.launch_request_path() {
            return Err(runtime_unavailable(
                "supervisor request path is not the derived session request",
            ));
        }
        Ok(())
    }

    fn validate_request_for_context(
        request: &LaunchRequest,
        context: &RuntimeContext,
    ) -> Result<()> {
        if request.schema_version != LAUNCH_REQUEST_SCHEMA
            || request.session_token.len() != 32
            || !request
                .session_token
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || request.session_key != context.session_key
            || request.bundle_root != context.bundle_root
            || (!context.bundle.manifest_sha256.is_empty()
                && request.bundle_manifest_sha256 != context.bundle.manifest_sha256)
            || (!context.bundle.product_commit.is_empty()
                && request.bundle_commit != context.bundle.product_commit)
            || !stable_workspace_matches_runtime(&request.workspace, &context.workspace)
            || request.listen_address != context.listen_address.to_string()
            || request.url != context.url()
            || request.runtime_root != context.runtime_root
            || request.session_directory != context.session_directory
            || (!context.bundle.workbench_sha256.is_empty()
                && request.workbench_sha256 != context.bundle.workbench_sha256)
        {
            return Err(runtime_unavailable(
                "launch request does not match the exact derived runtime",
            ));
        }
        Ok(())
    }

    fn parse_listen(value: &str) -> Result<SocketAddrV4> {
        let parsed: SocketAddrV4 = value
            .parse()
            .map_err(|_| runtime_unavailable("runtime listen address is invalid"))?;
        if *parsed.ip() != Ipv4Addr::LOCALHOST || parsed.port() == 0 {
            return Err(runtime_unavailable(
                "runtime listen address must be literal 127.0.0.1 with a non-zero port",
            ));
        }
        Ok(parsed)
    }

    fn stable_workspace_identity(workspace: &RuntimeWorkspaceIdentity) -> StableWorkspaceIdentity {
        StableWorkspaceIdentity {
            canonical_path: workspace.canonical_path.clone(),
            filesystem_identity_sha256: workspace.filesystem_identity_sha256.clone(),
            workspace_id: workspace
                .object_workspace
                .as_ref()
                .map(|record| record.workspace_id.clone()),
            descriptor_sha256: workspace
                .object_workspace
                .as_ref()
                .map(|record| record.descriptor_hash.clone()),
        }
    }

    fn stable_workspace_matches_runtime(
        stable: &StableWorkspaceIdentity,
        runtime: &RuntimeWorkspaceIdentity,
    ) -> bool {
        stable.canonical_path == runtime.canonical_path
            && stable.filesystem_identity_sha256 == runtime.filesystem_identity_sha256
            && stable.workspace_id.as_ref().is_none_or(|workspace_id| {
                runtime
                    .object_workspace
                    .as_ref()
                    .is_some_and(|record| &record.workspace_id == workspace_id)
            })
            && stable
                .descriptor_sha256
                .as_ref()
                .is_none_or(|descriptor_sha256| {
                    runtime
                        .object_workspace
                        .as_ref()
                        .is_some_and(|record| &record.descriptor_hash == descriptor_sha256)
                })
    }

    fn read_canonical_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
        verify_private_regular_file(path)?;
        let mut file = File::open(path).map_err(|error| KernelError::io(path, error))?;
        let metadata = file
            .metadata()
            .map_err(|error| KernelError::io(path, error))?;
        if metadata.len() == 0 || metadata.len() > MAXIMUM_RUNTIME_JSON_BYTES {
            return Err(runtime_unavailable(format!(
                "runtime JSON {} exceeds its bound",
                path.display()
            )));
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAXIMUM_RUNTIME_JSON_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| KernelError::io(path, error))?;
        parse_canonical_json(&bytes, "runtime JSON")
    }

    fn parse_canonical_json<T: DeserializeOwned>(bytes: &[u8], label: &str) -> Result<T> {
        let value = parse_unique_json(bytes)
            .map_err(|error| runtime_unavailable(format!("{label} is invalid: {error}")))?;
        if canonical_json_bytes(&value)? != bytes {
            return Err(runtime_unavailable(format!("{label} is not canonical")));
        }
        serde_json::from_value(value)
            .map_err(|error| runtime_unavailable(format!("{label} contract is invalid: {error}")))
    }

    fn write_new_canonical_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
        let bytes = canonical_json_bytes(&serde_json::to_value(value)?)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|error| KernelError::io(path, error))?;
        file.write_all(&bytes)
            .map_err(|error| KernelError::io(path, error))?;
        file.sync_all()
            .map_err(|error| KernelError::io(path, error))?;
        set_exact_mode(path, 0o600)
    }

    fn write_replace_canonical_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| runtime_unavailable("runtime JSON has no parent"))?;
        validate_secure_runtime_directory(parent)?;
        if let Ok(metadata) = fs::symlink_metadata(path)
            && (!metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.uid() != geteuid().as_raw())
        {
            return Err(runtime_unavailable(
                "runtime JSON replacement target has an unsafe identity",
            ));
        }
        let partial = parent.join(format!(
            ".{}.next-{}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("state"),
            Uuid::new_v4()
        ));
        let bytes = canonical_json_bytes(&serde_json::to_value(value)?)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)
            .map_err(|error| KernelError::io(&partial, error))?;
        file.write_all(&bytes)
            .map_err(|error| KernelError::io(&partial, error))?;
        file.sync_all()
            .map_err(|error| KernelError::io(&partial, error))?;
        set_exact_mode(&partial, 0o600)?;
        fs::rename(&partial, path).map_err(|error| KernelError::io(path, error))
    }

    fn verify_private_regular_file(path: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != geteuid().as_raw()
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(runtime_unavailable(format!(
                "runtime control file {} must be one mode-0600 private file",
                path.display()
            )));
        }
        Ok(())
    }

    fn verify_owned_socket(path: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
        if !metadata.file_type().is_socket()
            || metadata.file_type().is_symlink()
            || metadata.uid() != geteuid().as_raw()
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(runtime_unavailable(format!(
                "runtime control socket {} has an unsafe identity",
                path.display()
            )));
        }
        Ok(())
    }

    fn open_private_log(path: &Path, truncate: bool) -> Result<File> {
        if let Ok(metadata) = fs::symlink_metadata(path)
            && (!metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.uid() != geteuid().as_raw()
                || metadata.nlink() != 1)
        {
            return Err(runtime_unavailable(format!(
                "runtime log {} has an unsafe identity",
                path.display()
            )));
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(truncate)
            .append(!truncate)
            .mode(0o600)
            .open(path)
            .map_err(|error| KernelError::io(path, error))?;
        set_exact_mode(path, 0o600)?;
        Ok(file)
    }

    fn set_exact_mode(path: &Path, mode: u32) -> Result<()> {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|error| KernelError::io(path, error))
    }

    fn token_hash(value: &str) -> String {
        format!("{:x}", Sha256::digest(value.as_bytes()))
    }

    fn is_lower_hex_sha256(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    fn runtime_conflict(message: impl Into<String>) -> KernelError {
        KernelError::RuntimeConflict(message.into())
    }

    fn runtime_unavailable(message: impl Into<String>) -> KernelError {
        KernelError::RuntimeUnavailable(message.into())
    }

    #[cfg(test)]
    mod tests {
        use std::io::{BufRead as _, BufReader};

        use super::*;

        #[test]
        fn device_id_normalization_is_portable_and_fail_closed() {
            assert_eq!(normalized_device_id(42_u64), Some(42));
            assert_eq!(normalized_device_id(42_i32), Some(42));
            assert_eq!(normalized_device_id(-1_i32), None);
        }

        fn test_runtime_context(root: &Path) -> RuntimeContext {
            use crate::runtime_bundle::{BundleSourceState, BundleTarget};

            let runtime_root = root.join("runtime");
            let session_directory = runtime_root.join("session-test");
            let workspace = root.join("workspace");
            let bundle_root = root.join("bundle");
            for path in [&runtime_root, &session_directory, &workspace, &bundle_root] {
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(path)
                    .expect("create private lifecycle fixture directory");
            }
            RuntimeContext {
                bundle: BundleVerification {
                    bundle_root: bundle_root.clone(),
                    manifest_sha256: "a".repeat(64),
                    product_commit: "b".repeat(40),
                    product_version: "test".to_owned(),
                    source_state: BundleSourceState::Clean,
                    source_snapshot_sha256: "c".repeat(64),
                    target: BundleTarget {
                        os: "macos".to_owned(),
                        architecture: "arm64".to_owned(),
                        target_triple: "aarch64-apple-darwin".to_owned(),
                        minimum_os_version: Some("14.0".to_owned()),
                    },
                    launcher_sha256: "d".repeat(64),
                    runtime_controller_sha256: "e".repeat(64),
                    integrity_cli_sha256: "f".repeat(64),
                    workbench_sha256: "1".repeat(64),
                    member_count: 1,
                    total_member_bytes: 1,
                },
                bundle_root,
                current_executable: PathBuf::from("/bin/false"),
                workspace: RuntimeWorkspaceIdentity {
                    canonical_path: workspace,
                    filesystem_identity_sha256:
                        super::super::unix_workspace_filesystem_identity_sha256(7, 11),
                    object_workspace_state:
                        crate::object_workspace::ObjectWorkspaceState::InitializeRequired,
                    object_workspace: None,
                },
                listen_address: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9),
                runtime_root,
                session_directory,
                session_key: "2".repeat(64),
            }
        }

        fn test_control_response(context: &RuntimeContext, recorded_group: u32) -> ControlResponse {
            ControlResponse {
                schema_version: CONTROL_RESPONSE_SCHEMA.to_owned(),
                accepted: true,
                state: SupervisorState::Stopping,
                session_key: context.session_key.clone(),
                bundle_root: context.bundle_root.clone(),
                bundle_manifest_sha256: context.bundle.manifest_sha256.clone(),
                bundle_commit: context.bundle.product_commit.clone(),
                workspace: stable_workspace_identity(&context.workspace),
                listen_address: context.listen_address.to_string(),
                url: context.url(),
                supervisor_pid: recorded_group,
                workbench_pid: recorded_group.saturating_add(1),
                workbench_process_group_id: recorded_group,
                workbench_sha256: context.bundle.workbench_sha256.clone(),
                log_path: context.session_directory.join(WORKBENCH_LOG_FILENAME),
                application_identity_sha256: None,
                reason: None,
            }
        }

        fn test_launch_request(context: &RuntimeContext) -> LaunchRequest {
            LaunchRequest {
                schema_version: LAUNCH_REQUEST_SCHEMA.to_owned(),
                session_token: "3".repeat(32),
                session_key: context.session_key.clone(),
                bundle_root: context.bundle_root.clone(),
                bundle_manifest_sha256: context.bundle.manifest_sha256.clone(),
                bundle_commit: context.bundle.product_commit.clone(),
                workspace: stable_workspace_identity(&context.workspace),
                listen_address: context.listen_address.to_string(),
                url: context.url(),
                runtime_root: context.runtime_root.clone(),
                session_directory: context.session_directory.clone(),
                workbench_sha256: context.bundle.workbench_sha256.clone(),
                created_at: "2026-01-01T00:00:00Z".to_owned(),
            }
        }

        fn wait_for_shell_ready(child: &mut Child) {
            let stdout = child.stdout.take().expect("ready stdout");
            let mut line = String::new();
            BufReader::new(stdout)
                .read_line(&mut line)
                .expect("read child readiness");
            assert_eq!(line.trim(), "ready");
        }

        #[test]
        fn session_key_binds_stable_workspace_filesystem_identity_without_paths_or_values() {
            let restart_identity = super::super::unix_workspace_filesystem_identity_sha256(7, 11);
            let first = session_key(&restart_identity);
            let relocated = session_key(&restart_identity);
            let same = session_key(&super::super::unix_workspace_filesystem_identity_sha256(
                7, 11,
            ));
            let changed = session_key(&super::super::unix_workspace_filesystem_identity_sha256(
                7, 12,
            ));
            assert_eq!(first, same);
            assert_eq!(
                first, relocated,
                "a path relocation cannot affect the session key"
            );
            assert_ne!(first, changed);
            assert_eq!(first.len(), 64);
            assert!(!first.contains("workspace"));
        }

        #[test]
        fn filesystem_identity_digest_is_stable_and_changes_for_native_replacement() {
            let first = super::super::unix_workspace_filesystem_identity_sha256(7, 11);
            assert_eq!(
                first,
                super::super::unix_workspace_filesystem_identity_sha256(7, 11)
            );
            assert_ne!(
                first,
                super::super::unix_workspace_filesystem_identity_sha256(7, 12)
            );
            assert_eq!(first.len(), 64);
        }

        #[test]
        fn launch_protocol_v2_rejects_v1_missing_and_altered_workspace_identity() {
            let temporary = tempfile::tempdir().expect("identity protocol fixture");
            let context = test_runtime_context(temporary.path());
            let request = test_launch_request(&context);
            assert!(validate_request_for_context(&request, &context).is_ok());

            let mut v1 = request.clone();
            v1.schema_version =
                "migration-stream/assisted-local-runtime/launch-request/v1".to_owned();
            assert!(
                validate_request_for_context(&v1, &context).is_err(),
                "v1 replay must fail closed"
            );

            let mut request_json = serde_json::to_value(&request).expect("serialize v2 request");
            request_json
                .as_object_mut()
                .expect("request object")
                .get_mut("workspace")
                .and_then(serde_json::Value::as_object_mut)
                .expect("workspace object")
                .remove("filesystemIdentitySha256");
            assert!(
                serde_json::from_value::<LaunchRequest>(request_json).is_err(),
                "missing digest must fail parsing"
            );

            let mut control_json = serde_json::to_value(test_control_response(&context, 99))
                .expect("serialize control response");
            control_json
                .as_object_mut()
                .expect("control response object")
                .get_mut("workspace")
                .and_then(serde_json::Value::as_object_mut)
                .expect("control workspace object")
                .remove("filesystemIdentitySha256");
            assert!(
                serde_json::from_value::<ControlResponse>(control_json).is_err(),
                "missing control digest must fail parsing"
            );

            let mut altered_request = request.clone();
            altered_request.workspace.filesystem_identity_sha256 = "4".repeat(64);
            assert!(
                validate_request_for_context(&altered_request, &context).is_err(),
                "native-object digest mismatch must fail even at the same selected path"
            );
            let mut relocated_request = request.clone();
            relocated_request.workspace.canonical_path =
                temporary.path().join("relocated-workspace");
            assert!(
                validate_request_for_context(&relocated_request, &context).is_err(),
                "path mismatch must fail even with the same native-object digest"
            );
            let mut legacy_request = serde_json::to_value(&request).expect("serialize request");
            let legacy_workspace = legacy_request
                .as_object_mut()
                .expect("request object")
                .get_mut("workspace")
                .and_then(serde_json::Value::as_object_mut)
                .expect("workspace object");
            legacy_workspace.insert("filesystemDevice".to_owned(), serde_json::json!(999));
            legacy_workspace.insert("filesystemInode".to_owned(), serde_json::json!(1_000));
            assert!(
                serde_json::from_value::<LaunchRequest>(legacy_request).is_err(),
                "v2 launch requests must reject legacy numeric coordinates"
            );

            let mut legacy_public_workspace =
                serde_json::to_value(&context.workspace).expect("serialize public workspace");
            let legacy_workspace = legacy_public_workspace
                .as_object_mut()
                .expect("public workspace object");
            legacy_workspace.insert("filesystemDevice".to_owned(), serde_json::json!(999));
            legacy_workspace.insert("filesystemInode".to_owned(), serde_json::json!(1_000));
            assert!(
                serde_json::from_value::<RuntimeWorkspaceIdentity>(legacy_public_workspace)
                    .is_err(),
                "public v2 workspace identity must reject legacy numeric coordinates"
            );
        }

        #[test]
        fn readiness_and_control_protocol_v2_reject_identity_spoofing() {
            let temporary = tempfile::tempdir().expect("identity protocol fixture");
            let context = test_runtime_context(temporary.path());
            let request = test_launch_request(&context);

            let readiness = RuntimeReadiness {
                schema_version: "migration-stream/assisted-local-runtime/readiness/v2".to_owned(),
                session_token: request.session_token.clone(),
                process_id: 99,
                listen_address: request.listen_address.clone(),
                workspace_path: request.workspace.canonical_path.clone(),
                filesystem_identity_sha256: request.workspace.filesystem_identity_sha256.clone(),
            };
            assert!(runtime_readiness_matches(&readiness, &request, 99));
            let mut v1_readiness = readiness.clone();
            v1_readiness.schema_version =
                "migration-stream/assisted-local-runtime/readiness/v1".to_owned();
            assert!(!runtime_readiness_matches(&v1_readiness, &request, 99));
            let missing_readiness = serde_json::json!({
                "schemaVersion": "migration-stream/assisted-local-runtime/readiness/v2",
                "sessionToken": request.session_token.clone(), "processId": 99,
                "listenAddress": request.listen_address.clone(), "workspacePath": request.workspace.canonical_path.clone(),
            });
            assert!(
                serde_json::from_value::<RuntimeReadiness>(missing_readiness).is_err(),
                "missing readiness digest must fail parsing"
            );
            let mut altered_readiness = readiness.clone();
            altered_readiness.filesystem_identity_sha256 = "4".repeat(64);
            assert!(!runtime_readiness_matches(&altered_readiness, &request, 99));
            let mut relocated_readiness = readiness.clone();
            relocated_readiness.workspace_path = temporary.path().join("relocated-workspace");
            assert!(
                !runtime_readiness_matches(&relocated_readiness, &request, 99),
                "readiness path is a separate required startup-scope binding"
            );

            let legacy_readiness = serde_json::json!({
                "schemaVersion": "migration-stream/assisted-local-runtime/readiness/v2",
                "sessionToken": request.session_token.clone(), "processId": 99,
                "listenAddress": request.listen_address.clone(), "workspacePath": request.workspace.canonical_path.clone(),
                "filesystemIdentitySha256": request.workspace.filesystem_identity_sha256.clone(),
                "filesystemDevice": 999, "filesystemInode": 1_000,
            });
            assert!(
                serde_json::from_value::<RuntimeReadiness>(legacy_readiness).is_err(),
                "v2 readiness must reject legacy numeric coordinates"
            );

            let mut response = test_control_response(&context, 99);
            response.workspace.filesystem_identity_sha256 = "4".repeat(64);
            assert!(
                validate_control_response(&context, &request, &response).is_err(),
                "altered control identity must fail"
            );
            let mut relocated_response = test_control_response(&context, 99);
            relocated_response.workspace.canonical_path =
                temporary.path().join("relocated-workspace");
            assert!(
                validate_control_response(&context, &request, &relocated_response).is_err(),
                "control path scope mismatch must fail even with the same digest"
            );
            let mut legacy_response = serde_json::to_value(test_control_response(&context, 99))
                .expect("serialize control response");
            let legacy_workspace = legacy_response
                .as_object_mut()
                .expect("control response object")
                .get_mut("workspace")
                .and_then(serde_json::Value::as_object_mut)
                .expect("control workspace object");
            legacy_workspace.insert("filesystemDevice".to_owned(), serde_json::json!(999));
            legacy_workspace.insert("filesystemInode".to_owned(), serde_json::json!(1_000));
            assert!(
                serde_json::from_value::<ControlResponse>(legacy_response).is_err(),
                "v2 control responses must reject legacy numeric coordinates"
            );
            let mut v1_response = test_control_response(&context, 99);
            v1_response.schema_version =
                "migration-stream/assisted-local-runtime/control-response/v1".to_owned();
            assert!(
                validate_control_response(&context, &request, &v1_response).is_err(),
                "v1 control replay must fail closed"
            );
        }

        #[test]
        fn application_identity_digest_is_authoritative_and_rejects_legacy_coordinates() {
            let temporary = tempfile::tempdir().expect("application identity fixture");
            let context = test_runtime_context(temporary.path());
            let request = test_launch_request(&context);
            let identity = RuntimeHttpIdentity {
                schema_version: APPLICATION_IDENTITY_SCHEMA.to_owned(),
                session_token_sha256: token_hash(&request.session_token),
                process_id: 99,
                listen_address: request.listen_address.clone(),
                workspace_path: request.workspace.canonical_path.clone(),
                filesystem_identity_sha256: request.workspace.filesystem_identity_sha256.clone(),
                application_identity_sha256: "5".repeat(64),
            };
            assert!(runtime_http_identity_matches(&identity, &request, 99));

            let missing_identity = serde_json::json!({
                "schemaVersion": APPLICATION_IDENTITY_SCHEMA,
                "sessionTokenSha256": token_hash(&request.session_token), "processId": 99,
                "listenAddress": request.listen_address.clone(), "workspacePath": request.workspace.canonical_path.clone(),
                "applicationIdentitySha256": "5".repeat(64),
            });
            assert!(
                serde_json::from_value::<RuntimeHttpIdentity>(missing_identity).is_err(),
                "missing application digest must fail parsing"
            );

            let mut altered = identity.clone();
            altered.filesystem_identity_sha256 = "4".repeat(64);
            assert!(
                !runtime_http_identity_matches(&altered, &request, 99),
                "altered application digest must fail"
            );

            let mut relocated = identity.clone();
            relocated.workspace_path = temporary.path().join("relocated-workspace");
            assert!(
                !runtime_http_identity_matches(&relocated, &request, 99),
                "application path is a separate required startup-scope binding"
            );

            let mut v1 = identity.clone();
            v1.schema_version =
                "migration-stream/assisted-local-runtime/application-identity/v1".to_owned();
            assert!(
                !runtime_http_identity_matches(&v1, &request, 99),
                "v1 application replay must fail closed"
            );

            let legacy_identity = serde_json::json!({
                "schemaVersion": APPLICATION_IDENTITY_SCHEMA,
                "sessionTokenSha256": token_hash(&request.session_token), "processId": 99,
                "listenAddress": request.listen_address.clone(), "workspacePath": request.workspace.canonical_path.clone(),
                "filesystemIdentitySha256": request.workspace.filesystem_identity_sha256.clone(),
                "filesystemDevice": 999, "filesystemInode": 1_000,
                "applicationIdentitySha256": "5".repeat(64),
            });
            assert!(
                serde_json::from_value::<RuntimeHttpIdentity>(legacy_identity).is_err(),
                "v2 application identity must reject legacy numeric coordinates"
            );
        }

        #[test]
        fn listen_parser_accepts_only_literal_ipv4_loopback() {
            assert!(parse_listen("127.0.0.1:4173").is_ok());
            for value in [
                "0.0.0.0:4173",
                "127.0.0.2:4173",
                "[::1]:4173",
                "localhost:4173",
                "127.0.0.1:0",
            ] {
                assert!(parse_listen(value).is_err(), "{value} must fail");
            }
        }

        #[test]
        fn linux_xdg_candidate_requires_a_real_private_owned_directory() {
            let fixture_base = if cfg!(target_os = "macos") {
                Path::new("/private/tmp")
            } else {
                Path::new("/tmp")
            };
            let temporary = tempfile::Builder::new()
                .prefix("migration-stream-runtime-base-")
                .tempdir_in(fixture_base)
                .expect("runtime-base fixture");
            let private = temporary.path().join("private-xdg");
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&private)
                .expect("private XDG fixture");
            assert_eq!(
                verify_owned_private_real_directory(&private).expect("valid XDG candidate"),
                private.canonicalize().expect("canonical private fixture")
            );

            fs::set_permissions(&private, fs::Permissions::from_mode(0o755))
                .expect("relax fixture mode");
            assert!(verify_owned_private_real_directory(&private).is_err());

            fs::set_permissions(&private, fs::Permissions::from_mode(0o700))
                .expect("restore fixture mode");

            let linked = temporary.path().join("linked-xdg");
            std::os::unix::fs::symlink(&private, &linked).expect("linked XDG fixture");
            assert!(verify_owned_private_real_directory(&linked).is_err());

            assert_eq!(
                select_linux_runtime_base(Some(private.clone()), Path::new("/tmp"))
                    .expect("select valid XDG candidate"),
                private
                    .canonicalize()
                    .expect("canonical selected XDG candidate")
            );
        }

        #[test]
        fn supervisor_inherits_the_exact_owner_lock_until_exit() {
            let temporary = tempfile::tempdir().expect("owner-lock fixture");
            let path = temporary.path().join("owner.lock");
            let owner = open_private_lock(&path).expect("open owner lock");
            owner.lock_exclusive().expect("lock owner identity");
            let inherited = owner.try_clone().expect("clone owner lock");
            let mut child = Command::new("/bin/sh")
                .arg("-c")
                .arg("sleep 1")
                .stdin(Stdio::from(inherited))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn lock-owning child");
            drop(owner);

            let observer = open_private_lock(&path).expect("open owner-lock observer");
            let blocked = observer
                .try_lock_exclusive()
                .expect_err("child must retain the inherited owner lock");
            assert_eq!(blocked.kind(), std::io::ErrorKind::WouldBlock);

            let status = child.wait().expect("reap lock-owning child");
            assert!(status.success());
            observer
                .try_lock_exclusive()
                .expect("owner lock must release when the child exits");
            fs2::FileExt::unlock(&observer).expect("unlock observer");
        }

        #[test]
        fn supervisor_rejects_wrong_or_unlocked_owner_descriptor() {
            let temporary = tempfile::tempdir().expect("owner-descriptor fixture");
            let owner_path = temporary.path().join(OWNER_LOCK_FILENAME);
            let owner = open_private_lock(&owner_path).expect("open exact owner file");
            assert!(
                validate_owner_lock_descriptor(temporary.path(), &owner).is_err(),
                "an unlocked exact descriptor must fail"
            );

            owner.lock_exclusive().expect("lock exact owner file");
            let wrong_path = temporary.path().join("wrong.lock");
            let wrong = open_private_lock(&wrong_path).expect("open wrong owner file");
            assert!(
                validate_owner_lock_descriptor(temporary.path(), &wrong).is_err(),
                "a locked owner name with the wrong inherited descriptor must fail"
            );
            validate_owner_lock_descriptor(temporary.path(), &owner)
                .expect("the exact locked inherited descriptor must pass");
            fs2::FileExt::unlock(&owner).expect("unlock exact owner");
        }

        #[test]
        fn runtime_identity_digest_requires_exact_lowercase_sha256() {
            assert!(is_lower_hex_sha256(&"a".repeat(64)));
            assert!(!is_lower_hex_sha256(&"A".repeat(64)));
            assert!(!is_lower_hex_sha256(&"a".repeat(63)));
            assert!(!is_lower_hex_sha256(&format!("{}g", "a".repeat(63))));
        }

        #[test]
        fn runtime_child_environment_is_exact_and_session_local() {
            let temporary = tempfile::tempdir().expect("runtime environment fixture");
            let mut command = Command::new("/usr/bin/env");
            configure_runtime_temp_environment(&mut command, temporary.path());
            let output = command.output().expect("read isolated child environment");
            assert!(output.status.success());
            let mut lines = String::from_utf8(output.stdout)
                .expect("environment output UTF-8")
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            lines.sort();
            let path = temporary.path().display().to_string();
            assert_eq!(
                lines,
                vec![format!("SQLITE_TMPDIR={path}"), format!("TMPDIR={path}")]
            );
        }

        #[test]
        fn runtime_temp_cleanup_is_bounded_and_does_not_follow_links() {
            use std::os::unix::fs::symlink;

            let temporary = tempfile::tempdir().expect("runtime temp fixture");
            let session = temporary.path().join("session");
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&session)
                .expect("create private session");
            let runtime_temp = session.join(SESSION_TEMP_DIRECTORY_NAME);
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&runtime_temp)
                .expect("create private runtime temp");
            fs::write(runtime_temp.join("sqlite-temp"), b"bounded").expect("write temp file");
            let outside = temporary.path().join("outside");
            fs::write(&outside, b"must survive").expect("write outside sentinel");
            symlink(&outside, runtime_temp.join("outside-link")).expect("link outside sentinel");

            cleanup_runtime_temp_directory(&session).expect("clean exact runtime temp");
            assert!(!runtime_temp.exists());
            assert_eq!(
                fs::read(&outside).expect("outside sentinel survives"),
                b"must survive"
            );
        }

        #[tokio::test]
        async fn bounded_cleanup_kills_and_reaps_noncooperative_child() {
            let mut command = Command::new("/bin/sh");
            command
                .arg("-c")
                .arg("trap '' TERM; echo ready; while :; do :; done")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0);
            let mut child = command.spawn().expect("spawn noncooperative child");
            wait_for_shell_ready(&mut child);
            let stdin = child.stdin.take();
            let process_group = pid_from_u32(child.id()).expect("process group");
            let status = terminate_owned_child_bounded(
                &mut child,
                stdin,
                Duration::from_millis(100),
                Duration::from_secs(2),
            )
            .await
            .expect("bounded cleanup");
            assert!(!status.success());
            assert!(test_kill_process_group(process_group).is_err());
        }

        #[test]
        fn failed_start_kills_the_live_supervisor_group_and_all_noncooperative_children() {
            let mut command = Command::new("/bin/sh");
            command
                .arg("-c")
                .arg("trap '' TERM; (trap '' TERM; while :; do :; done) & echo ready; wait")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0);
            let mut supervisor = command.spawn().expect("spawn startup supervisor fixture");
            wait_for_shell_ready(&mut supervisor);
            let supervisor_group = pid_from_u32(supervisor.id()).expect("supervisor group");

            let cleanup = terminate_live_startup_group(
                &mut supervisor,
                supervisor_group,
                Duration::from_secs(2),
                Path::new("/bin/sh"),
            );
            if cleanup.is_err() {
                let _ = kill_process_group(supervisor_group, Signal::KILL);
                let _ = supervisor.wait();
            }
            cleanup.expect("live startup group cleanup");
            observe_process_group_extinct(supervisor_group, Duration::from_secs(2))
                .expect("startup group extinction");
        }

        #[tokio::test]
        async fn post_start_wait_never_signals_recorded_or_reused_process_groups() {
            let temporary = tempfile::tempdir().expect("post-start no-signal fixture");
            let context = test_runtime_context(temporary.path());
            let owner = open_private_lock(&context.owner_lock_path()).expect("owner lock");
            owner.lock_exclusive().expect("hold foreign owner sentinel");

            let mut command = Command::new("/bin/sh");
            command
                .arg("-c")
                .arg("trap '' TERM; echo ready; while :; do :; done")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0);
            let mut foreign = command
                .spawn()
                .expect("spawn foreign process-group sentinel");
            wait_for_shell_ready(&mut foreign);
            let foreign_group = pid_from_u32(foreign.id()).expect("foreign group");
            let response = test_control_response(&context, foreign.id());

            let result = wait_for_owned_stop_bounded(&context, &response, Duration::ZERO).await;
            let foreign_survived = foreign
                .try_wait()
                .expect("probe foreign sentinel")
                .is_none();
            let cleanup = terminate_live_startup_group(
                &mut foreign,
                foreign_group,
                Duration::from_secs(2),
                Path::new("/bin/sh"),
            );
            fs2::FileExt::unlock(&owner).expect("release owner sentinel");

            assert!(result.is_err(), "active owner must keep stop bounded");
            assert!(
                foreign_survived,
                "recorded numeric process identities must never be signalled post-start"
            );
            cleanup.expect("clean foreign test sentinel through its live Child");
        }
    }
}

#[cfg(unix)]
pub use unix::{
    integrity_runtime, restart_runtime, run_supervisor, start_runtime, status_runtime, stop_runtime,
};

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{
    integrity_runtime, restart_runtime, run_supervisor, start_runtime, status_runtime, stop_runtime,
};

#[cfg(not(any(unix, windows)))]
pub async fn start_runtime(_options: &RuntimeCommandOptions) -> Result<RuntimeStatusReport> {
    Err(KernelError::BundlePlatform(
        "this target lacks the native assisted-runtime lifecycle adapter".to_owned(),
    ))
}

#[cfg(not(any(unix, windows)))]
pub fn status_runtime(_options: &RuntimeCommandOptions) -> Result<RuntimeStatusReport> {
    Err(KernelError::BundlePlatform(
        "this target lacks the native assisted-runtime lifecycle adapter".to_owned(),
    ))
}

#[cfg(not(any(unix, windows)))]
pub async fn restart_runtime(_options: &RuntimeCommandOptions) -> Result<RuntimeStatusReport> {
    Err(KernelError::BundlePlatform(
        "this target lacks the native assisted-runtime lifecycle adapter".to_owned(),
    ))
}

#[cfg(not(any(unix, windows)))]
pub fn integrity_runtime(_options: &RuntimeCommandOptions) -> Result<RuntimeIntegrityReport> {
    Err(KernelError::BundlePlatform(
        "this target lacks the native assisted-runtime lifecycle adapter".to_owned(),
    ))
}

#[cfg(not(any(unix, windows)))]
pub async fn stop_runtime(_options: &RuntimeCommandOptions) -> Result<RuntimeStatusReport> {
    Err(KernelError::BundlePlatform(
        "this target lacks the native assisted-runtime lifecycle adapter".to_owned(),
    ))
}

#[cfg(not(any(unix, windows)))]
pub async fn run_supervisor(_request_path: &std::path::Path) -> Result<()> {
    Err(KernelError::BundlePlatform(
        "this target lacks the native assisted-runtime lifecycle adapter".to_owned(),
    ))
}

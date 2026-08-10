//! Native Windows lifecycle adapter.
//!
//! All Win32 handle operations remain in the separately audited systems
//! crate. This layer owns only canonical product protocol and lifecycle state.

use std::{
    ffi::{OsStr, OsString},
    io::{Read as _, Write as _},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use migration_stream_windows_native::{
    AuthenticatedPipeClient, AuthenticatedPipeResponse, AuthenticatedPipeServer,
    AuthenticatedReadinessPermit, CapabilityToken, CurrentUser, KillOnCloseJob, ManagedProcess,
    OwnedProcess, PrivateLock, PrivateRuntimeDirectory, RealDirectory, RealFile, SessionNonce,
    SystemRootDirectory, authenticated_control_binding, local_app_data_directory,
    private_lock_is_contended, system_root_directory, windows_workspace_filesystem_identity_sha256,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest as _, Sha256};

use super::{
    RUNTIME_INTEGRITY_SCHEMA, RUNTIME_STATUS_SCHEMA, RuntimeClassification, RuntimeCommandOptions,
    RuntimeIntegrityReport, RuntimeStatusReport, RuntimeWorkspaceIdentity,
};
use crate::{
    KernelError, Result,
    canonical::{canonical_json_bytes, parse_unique_json},
    object_workspace::ObjectWorkspaceService,
    runtime_bundle::{BundleVerification, verify_bundle_root},
    state::integrity_check,
};

const WINDOWS_LAUNCH_REQUEST_SCHEMA: &str =
    "migration-stream/assisted-local-runtime/windows-launch-request/v2";
const WINDOWS_SESSION_RECORD_SCHEMA: &str =
    "migration-stream/assisted-local-runtime/windows-session/v2";
const WINDOWS_CONTROL_REQUEST_SCHEMA: &str =
    "migration-stream/assisted-local-runtime/windows-control-request/v2";
const WINDOWS_CONTROL_RESPONSE_SCHEMA: &str =
    "migration-stream/assisted-local-runtime/windows-control-response/v2";
const APPLICATION_IDENTITY_SCHEMA: &str =
    "migration-stream/assisted-local-runtime/application-identity/v2";

// Native private-component grammar deliberately excludes dots and separators.
const RUNTIME_ROOT_COMPONENT: &str = "migration-stream-runtime";
const LAUNCH_REQUEST_COMPONENT: &str = "launch-request-json";
const SESSION_RECORD_COMPONENT: &str = "session-json";
const COMMAND_LOCK_COMPONENT: &str = "command-lock";
const OWNER_LOCK_COMPONENT: &str = "owner-lock";
const SESSION_TEMP_COMPONENT: &str = "session-temp";

const MAXIMUM_STATE_BYTES: usize = 128 * 1024;
const ADOPTION_LEASE: Duration = Duration::from_secs(15);
const START_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const COMMAND_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const LISTENER_PROBE_TIMEOUT: Duration = Duration::from_millis(250);

struct RuntimeContext {
    bundle: BundleVerification,
    bundle_root: Rc<RealDirectory>,
    entrypoint: Rc<RealFile>,
    launcher: Rc<RealFile>,
    controller: Rc<RealFile>,
    workbench: Rc<RealFile>,
    integrity_cli: Rc<RealFile>,
    // Retain every no-delete workspace path-component handle for this command.
    _workspace_directory: Rc<RealDirectory>,
    workspace: RuntimeWorkspaceIdentity,
    listen_address: SocketAddrV4,
    runtime_root: PrivateRuntimeDirectory,
    session: PrivateRuntimeDirectory,
    session_temp: PrivateRuntimeDirectory,
    user: Rc<CurrentUser>,
    system_root: SystemRootDirectory,
    session_key: String,
    pipe_component: String,
}

impl RuntimeContext {
    fn url(&self) -> String {
        format!("http://{}", self.listen_address)
    }

    fn launch_request_path(&self) -> PathBuf {
        self.session
            .directory()
            .path()
            .join(LAUNCH_REQUEST_COMPONENT)
    }

    fn runtime_root_path(&self) -> PathBuf {
        self.runtime_root.directory().path().to_path_buf()
    }

    fn session_path(&self) -> PathBuf {
        self.session.directory().path().to_path_buf()
    }
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LaunchRequest {
    schema_version: String,
    capability: String,
    session_nonce: String,
    session_key: String,
    pipe_component: String,
    bundle_root: PathBuf,
    bundle_manifest_sha256: String,
    bundle_commit: String,
    workspace: StableWorkspaceIdentity,
    listen_address: String,
    url: String,
    runtime_root: PathBuf,
    session_directory: PathBuf,
    controller_sha256: String,
    workbench_sha256: String,
    issued_unix_millis: u64,
    adoption_deadline_unix_millis: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SessionRecord {
    schema_version: String,
    capability: String,
    session_nonce: String,
    session_key: String,
    pipe_component: String,
    bundle_root: PathBuf,
    bundle_manifest_sha256: String,
    bundle_commit: String,
    workspace: StableWorkspaceIdentity,
    listen_address: String,
    url: String,
    supervisor_pid: u32,
    workbench_pid: u32,
    workbench_sha256: String,
    application_identity_sha256: String,
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
    Adopt,
    Status,
    Stop,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControlRequest {
    schema_version: String,
    operation: ControlOperation,
    session_key: String,
    session_nonce: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControlResponse {
    schema_version: String,
    accepted: bool,
    state: SupervisorState,
    session_key: String,
    session_nonce: String,
    bundle_root: PathBuf,
    bundle_manifest_sha256: String,
    bundle_commit: String,
    workspace: StableWorkspaceIdentity,
    listen_address: String,
    url: String,
    supervisor_pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    workbench_pid: Option<u32>,
    workbench_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    application_identity_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
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
    response: Option<ControlResponse>,
    reason: Option<String>,
}

enum ProtectedRecord {
    Launch { record: LaunchRequest },
    Session { record: SessionRecord },
}

impl ProtectedRecord {
    fn capability(&self) -> &str {
        match self {
            Self::Launch { record } => &record.capability,
            Self::Session { record } => &record.capability,
        }
    }

    fn nonce(&self) -> &str {
        match self {
            Self::Launch { record } => &record.session_nonce,
            Self::Session { record } => &record.session_nonce,
        }
    }
}

// Keep entrypoint, bundle, workspace and private-state identity establishment
// together: splitting this authority boundary would make TOCTOU review harder.
#[allow(clippy::too_many_lines)]
fn resolve_context(options: &RuntimeCommandOptions) -> Result<RuntimeContext> {
    if options.port == 0 || !options.workspace.is_absolute() {
        return Err(runtime_conflict(
            "workspace and a non-zero loopback port must be explicit at startup",
        ));
    }
    let user = CurrentUser::capture().map_err(|error| native_error("current user SID", error))?;
    let current = std::env::current_exe()
        .map_err(|error| runtime_unavailable(format!("current executable: {error}")))?;
    let entrypoint = Rc::new(
        RealFile::capture(&current)
            .map_err(|error| native_error("runtime entrypoint identity", error))?,
    );
    let entrypoint_parent = entrypoint
        .path()
        .parent()
        .ok_or_else(|| runtime_unavailable("runtime entrypoint has no parent"))?;
    let invoked_as_launcher = match entrypoint.path().file_name().and_then(OsStr::to_str) {
        Some("migration-stream-local.exe") => true,
        Some("migration-stream-local-runtime.exe") => {
            require_ascii_file_name(entrypoint_parent, "libexec", "runtime controller directory")?;
            false
        }
        _ => {
            return Err(runtime_unavailable(
                "current executable is not an exact fixed Windows bundle entrypoint",
            ));
        }
    };
    let bundle_root_path = if invoked_as_launcher {
        entrypoint_parent
    } else {
        entrypoint_parent
            .parent()
            .ok_or_else(|| runtime_unavailable("runtime controller has no bundle root"))?
    };
    let bundle_root = Rc::new(
        RealDirectory::capture(bundle_root_path)
            .map_err(|error| native_error("bundle root identity", error))?,
    );
    let launcher = Rc::new(
        RealFile::capture(&bundle_root.path().join("migration-stream-local.exe"))
            .map_err(|error| native_error("bundled command launcher", error))?,
    );
    let controller = Rc::new(
        RealFile::capture(
            &bundle_root
                .path()
                .join("libexec/migration-stream-local-runtime.exe"),
        )
        .map_err(|error| native_error("bundled runtime controller", error))?,
    );
    let expected_entrypoint = if invoked_as_launcher {
        &launcher
    } else {
        &controller
    };
    if expected_entrypoint.identity() != entrypoint.identity() {
        return Err(runtime_unavailable(
            "current executable is not the exact bundled Windows entrypoint",
        ));
    }
    let bundle = verify_bundle_root(bundle_root.path())?;
    if bundle.launcher_sha256 != bundle.runtime_controller_sha256 {
        return Err(runtime_unavailable(
            "Windows launcher is not byte-identical to the exact native controller",
        ));
    }
    let workbench = Rc::new(
        RealFile::capture(
            &bundle_root
                .path()
                .join("bin/migration-stream-workbench.exe"),
        )
        .map_err(|error| native_error("bundled workbench identity", error))?,
    );
    let integrity_cli = Rc::new(
        RealFile::capture(&bundle_root.path().join("bin/migration-stream.exe"))
            .map_err(|error| native_error("bundled integrity CLI identity", error))?,
    );
    verify_bundle_replay(
        &bundle,
        &bundle_root,
        &entrypoint,
        &launcher,
        &controller,
        &workbench,
        &integrity_cli,
    )?;

    let workspace_directory = Rc::new(
        RealDirectory::capture(&options.workspace)
            .map_err(|error| native_error("workspace identity", error))?,
    );
    let workspace_status = ObjectWorkspaceService::new(workspace_directory.path())?.inspect()?;
    let workspace = RuntimeWorkspaceIdentity {
        canonical_path: workspace_directory.path().to_path_buf(),
        filesystem_identity_sha256: windows_workspace_filesystem_identity_sha256(
            workspace_directory.identity(),
        ),
        object_workspace_state: workspace_status.state,
        object_workspace: workspace_status.workspace,
    };
    if paths_overlap(bundle_root.path(), workspace_directory.path()) {
        return Err(runtime_conflict(
            "workspace and immutable bundle directories must not overlap",
        ));
    }

    let local_app_data = local_app_data_directory()
        .map_err(|error| native_error("LocalAppData authority", error))?;
    let runtime_root = PrivateRuntimeDirectory::open_or_create_child(
        local_app_data,
        Rc::clone(&user),
        RUNTIME_ROOT_COMPONENT,
    )
    .map_err(|error| native_error("private runtime root", error))?;
    if paths_overlap(bundle_root.path(), runtime_root.directory().path())
        || paths_overlap(workspace_directory.path(), runtime_root.directory().path())
    {
        return Err(runtime_conflict(
            "runtime metadata root must be outside bundle and workspace",
        ));
    }
    let session_key = session_key(&workspace.filesystem_identity_sha256);
    let session_component = format!("session-{}", &session_key[..24]);
    let session = PrivateRuntimeDirectory::open_or_create_child(
        runtime_root.directory(),
        Rc::clone(&user),
        &session_component,
    )
    .map_err(|error| native_error("private runtime session", error))?;
    let session_temp = PrivateRuntimeDirectory::open_or_create_child(
        session.directory(),
        Rc::clone(&user),
        SESSION_TEMP_COMPONENT,
    )
    .map_err(|error| native_error("private runtime temporary directory", error))?;
    let system_root =
        system_root_directory().map_err(|error| native_error("SystemRoot authority", error))?;
    let pipe_component = format!("control-{}", &session_key[..24]);
    Ok(RuntimeContext {
        bundle,
        bundle_root,
        entrypoint,
        launcher,
        controller,
        workbench,
        integrity_cli,
        _workspace_directory: workspace_directory,
        workspace,
        listen_address: options.listen_address(),
        runtime_root,
        session,
        session_temp,
        user,
        system_root,
        session_key,
        pipe_component,
    })
}

fn verify_bundle_replay(
    expected: &BundleVerification,
    root: &RealDirectory,
    entrypoint: &RealFile,
    launcher: &RealFile,
    controller: &RealFile,
    workbench: &RealFile,
    integrity_cli: &RealFile,
) -> Result<()> {
    root.verify()
        .and_then(|()| entrypoint.verify())
        .and_then(|()| launcher.verify())
        .and_then(|()| controller.verify())
        .and_then(|()| workbench.verify())
        .and_then(|()| integrity_cli.verify())
        .map_err(|error| native_error("bundle identity replay", error))?;
    let replay = verify_bundle_root(root.path())?;
    if replay.manifest_sha256 != expected.manifest_sha256
        || replay.launcher_sha256 != expected.launcher_sha256
        || replay.runtime_controller_sha256 != expected.runtime_controller_sha256
        || replay.workbench_sha256 != expected.workbench_sha256
        || replay.integrity_cli_sha256 != expected.integrity_cli_sha256
        || replay.launcher_sha256 != replay.runtime_controller_sha256
    {
        return Err(runtime_unavailable("bundle identity changed during replay"));
    }
    Ok(())
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

fn stable_workspace_matches(
    stable: &StableWorkspaceIdentity,
    workspace: &RuntimeWorkspaceIdentity,
) -> bool {
    stable.workspace_id.is_some() == stable.descriptor_sha256.is_some()
        && stable.canonical_path == workspace.canonical_path
        && stable.filesystem_identity_sha256 == workspace.filesystem_identity_sha256
        && stable.workspace_id.as_ref().is_none_or(|workspace_id| {
            workspace
                .object_workspace
                .as_ref()
                .is_some_and(|record| &record.workspace_id == workspace_id)
        })
        && stable
            .descriptor_sha256
            .as_ref()
            .is_none_or(|descriptor_sha256| {
                workspace
                    .object_workspace
                    .as_ref()
                    .is_some_and(|record| &record.descriptor_hash == descriptor_sha256)
            })
}

fn session_key(filesystem_identity_sha256: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"migration-stream-assisted-runtime-session-key/v2\0");
    digest.update(filesystem_identity_sha256.as_bytes());
    format!("{:x}", digest.finalize())
}

fn require_ascii_file_name(path: &Path, expected: &str, label: &str) -> Result<()> {
    let actual = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| runtime_unavailable(format!("{label} name is not canonical UTF-16")))?;
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(runtime_unavailable(format!("{label} is not {expected}")));
    }
    Ok(())
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn now_unix_millis() -> Result<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| runtime_unavailable("system clock predates the Unix epoch"))?
        .as_millis();
    u64::try_from(millis).map_err(|_| runtime_unavailable("system clock exceeds protocol bounds"))
}

fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    canonical_json_bytes(&serde_json::to_value(value)?)
}

fn parse_canonical<T: DeserializeOwned>(bytes: &[u8], label: &str) -> Result<T> {
    if bytes.is_empty() || bytes.len() > MAXIMUM_STATE_BYTES {
        return Err(runtime_unavailable(format!(
            "{label} exceeds its byte bound"
        )));
    }
    let value = parse_unique_json(bytes)
        .map_err(|error| runtime_unavailable(format!("{label} JSON is invalid: {error}")))?;
    if canonical_json_bytes(&value)? != bytes {
        return Err(runtime_unavailable(format!(
            "{label} JSON is not canonical"
        )));
    }
    serde_json::from_value(value)
        .map_err(|error| runtime_unavailable(format!("{label} contract is invalid: {error}")))
}

fn runtime_conflict(message: impl Into<String>) -> KernelError {
    KernelError::RuntimeConflict(message.into())
}

fn runtime_unavailable(message: impl Into<String>) -> KernelError {
    KernelError::RuntimeUnavailable(message.into())
}

// Error ownership keeps every native API adapter usable directly from
// `map_err` closures without changing its error or cleanup path.
#[allow(clippy::needless_pass_by_value)]
fn native_error(label: &str, error: std::io::Error) -> KernelError {
    runtime_unavailable(format!("{label}: {error}"))
}

fn lock_command(context: &RuntimeContext) -> Result<PrivateLock> {
    let deadline = Instant::now() + COMMAND_LOCK_TIMEOUT;
    loop {
        match context.session.open_and_lock(COMMAND_LOCK_COMPONENT) {
            Ok(lock) => return Ok(lock),
            Err(error) if private_lock_is_contended(&error) && Instant::now() < deadline => {
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(error) if private_lock_is_contended(&error) => {
                return Err(runtime_conflict(
                    "another lifecycle command still owns the bounded session lock",
                ));
            }
            Err(error) => return Err(native_error("runtime command lock", error)),
        }
    }
}

fn runtime_owner_is_active(context: &RuntimeContext) -> Result<bool> {
    match context.session.open_and_lock(OWNER_LOCK_COMPONENT) {
        Ok(lock) => {
            lock.verify()
                .map_err(|error| native_error("runtime owner lock", error))?;
            drop(lock);
            Ok(false)
        }
        Err(error) if private_lock_is_contended(&error) => Ok(true),
        Err(error) => Err(native_error("runtime owner lock", error)),
    }
}

fn load_protected_record(context: &RuntimeContext) -> Result<Option<ProtectedRecord>> {
    let (launch, session) = load_protected_record_pair(context)?;
    Ok(session
        .map(|(record, _)| ProtectedRecord::Session { record })
        .or_else(|| launch.map(|(record, _)| ProtectedRecord::Launch { record })))
}

type LaunchRecordBytes = (LaunchRequest, Vec<u8>);
type SessionRecordBytes = (SessionRecord, Vec<u8>);

fn load_protected_record_pair(
    context: &RuntimeContext,
) -> Result<(Option<LaunchRecordBytes>, Option<SessionRecordBytes>)> {
    let session = match context
        .session
        .read_state_file(SESSION_RECORD_COMPONENT, MAXIMUM_STATE_BYTES)
    {
        Ok(bytes) => {
            let record: SessionRecord = parse_canonical(&bytes, "Windows session record")?;
            validate_session_record(context, &record)?;
            Some((record, bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(native_error("Windows session record", error)),
    };
    let launch = match context
        .session
        .read_state_file(LAUNCH_REQUEST_COMPONENT, MAXIMUM_STATE_BYTES)
    {
        Ok(bytes) => {
            let record: LaunchRequest = parse_canonical(&bytes, "Windows launch request")?;
            validate_launch_request(context, &record, false)?;
            Some((record, bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(native_error("Windows launch request", error)),
    };
    if let (Some((launch, _)), Some((session, _))) = (&launch, &session)
        && (launch.capability != session.capability
            || launch.session_nonce != session.session_nonce
            || launch.session_key != session.session_key
            || launch.pipe_component != session.pipe_component)
    {
        return Err(runtime_unavailable(
            "paired Windows launch/session records have different authority",
        ));
    }
    Ok((launch, session))
}

fn validate_launch_request(
    context: &RuntimeContext,
    request: &LaunchRequest,
    enforce_adoption_lease: bool,
) -> Result<()> {
    CapabilityToken::parse_lowercase_hex(&request.capability)
        .map_err(|error| native_error("launch capability", error))?;
    SessionNonce::parse_lowercase_hex(&request.session_nonce)
        .map_err(|error| native_error("launch session nonce", error))?;
    let expected_lease = u64::try_from(ADOPTION_LEASE.as_millis())
        .map_err(|_| runtime_unavailable("adoption lease exceeds protocol bounds"))?;
    let lease_valid = request
        .adoption_deadline_unix_millis
        .checked_sub(request.issued_unix_millis)
        == Some(expected_lease);
    if request.schema_version != WINDOWS_LAUNCH_REQUEST_SCHEMA
        || request.session_key != context.session_key
        || request.pipe_component != context.pipe_component
        || request.bundle_root != context.bundle_root.path()
        || request.bundle_manifest_sha256 != context.bundle.manifest_sha256
        || request.bundle_commit != context.bundle.product_commit
        || !stable_workspace_matches(&request.workspace, &context.workspace)
        || request.listen_address != context.listen_address.to_string()
        || request.url != context.url()
        || request.runtime_root != context.runtime_root_path()
        || request.session_directory != context.session_path()
        || request.controller_sha256 != context.bundle.runtime_controller_sha256
        || request.workbench_sha256 != context.bundle.workbench_sha256
        || !lease_valid
        || (enforce_adoption_lease && request.adoption_deadline_unix_millis <= now_unix_millis()?)
    {
        return Err(runtime_unavailable(
            "Windows launch request does not match the exact derived runtime",
        ));
    }
    Ok(())
}

fn validate_session_record(context: &RuntimeContext, record: &SessionRecord) -> Result<()> {
    CapabilityToken::parse_lowercase_hex(&record.capability)
        .map_err(|error| native_error("session capability", error))?;
    SessionNonce::parse_lowercase_hex(&record.session_nonce)
        .map_err(|error| native_error("session nonce", error))?;
    if record.schema_version != WINDOWS_SESSION_RECORD_SCHEMA
        || record.session_key != context.session_key
        || record.pipe_component != context.pipe_component
        || record.bundle_root != context.bundle_root.path()
        || record.bundle_manifest_sha256 != context.bundle.manifest_sha256
        || record.bundle_commit != context.bundle.product_commit
        || !stable_workspace_matches(&record.workspace, &context.workspace)
        || record.listen_address != context.listen_address.to_string()
        || record.url != context.url()
        || record.supervisor_pid == 0
        || record.workbench_pid == 0
        || record.workbench_sha256 != context.bundle.workbench_sha256
        || !is_lowercase_sha256(&record.application_identity_sha256)
    {
        return Err(runtime_unavailable(
            "Windows session record does not match the exact derived runtime",
        ));
    }
    Ok(())
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn control_request(
    context: &RuntimeContext,
    record: &ProtectedRecord,
    operation: ControlOperation,
) -> Result<Vec<u8>> {
    canonical_bytes(&ControlRequest {
        schema_version: WINDOWS_CONTROL_REQUEST_SCHEMA.to_owned(),
        operation,
        session_key: context.session_key.clone(),
        session_nonce: record.nonce().to_owned(),
    })
}

fn authenticated_control(
    context: &RuntimeContext,
    record: &ProtectedRecord,
    operation: ControlOperation,
    deadline: Instant,
) -> Result<(AuthenticatedPipeResponse, ControlResponse, SessionNonce)> {
    let capability = CapabilityToken::parse_lowercase_hex(record.capability())
        .map_err(|error| native_error("protected control capability", error))?;
    let nonce = SessionNonce::parse_lowercase_hex(record.nonce())
        .map_err(|error| native_error("protected control nonce", error))?;
    let client = AuthenticatedPipeClient::connect(
        Rc::clone(&context.user),
        &context.pipe_component,
        capability,
        deadline,
    )
    .map_err(|error| native_error("authenticated control connection", error))?;
    let request = control_request(context, record, operation)?;
    let native_response = client
        .transact(&request)
        .map_err(|error| native_error("authenticated control transaction", error))?;
    let response: ControlResponse =
        parse_canonical(native_response.payload(), "Windows control response")?;
    validate_control_response(context, record, &response)?;
    Ok((native_response, response, nonce))
}

fn validate_control_response(
    context: &RuntimeContext,
    record: &ProtectedRecord,
    response: &ControlResponse,
) -> Result<()> {
    if response.schema_version != WINDOWS_CONTROL_RESPONSE_SCHEMA
        || response.session_key != context.session_key
        || response.session_nonce != record.nonce()
        || response.bundle_root != context.bundle_root.path()
        || response.bundle_manifest_sha256 != context.bundle.manifest_sha256
        || response.bundle_commit != context.bundle.product_commit
        || !stable_workspace_matches(&response.workspace, &context.workspace)
        || response.listen_address != context.listen_address.to_string()
        || response.url != context.url()
        || response.supervisor_pid == 0
        || !response_process_ids_match(record, response)
        || response.workbench_sha256 != context.bundle.workbench_sha256
        || response.workbench_pid == Some(0)
        || response
            .application_identity_sha256
            .as_deref()
            .is_some_and(|identity| !is_lowercase_sha256(identity))
        || (response.state == SupervisorState::Ready
            && (response.workbench_pid.is_none() || response.application_identity_sha256.is_none()))
    {
        return Err(runtime_unavailable(
            "authenticated Windows control response identity differs",
        ));
    }
    Ok(())
}

fn response_process_ids_match(record: &ProtectedRecord, response: &ControlResponse) -> bool {
    match record {
        ProtectedRecord::Launch { .. } => true,
        ProtectedRecord::Session { record } => {
            response.supervisor_pid == record.supervisor_pid
                && response.workbench_pid == Some(record.workbench_pid)
        }
    }
}

fn classify_runtime(context: &RuntimeContext) -> Result<ClassifiedRuntime> {
    let Some(record) = load_protected_record(context)? else {
        let listener_active = listener_is_active(context.listen_address);
        let owner_active = runtime_owner_is_active(context)?;
        return Ok(ClassifiedRuntime {
            classification: if listener_active {
                RuntimeClassification::ForeignListener
            } else if owner_active {
                RuntimeClassification::Starting
            } else {
                RuntimeClassification::Stopped
            },
            response: None,
            reason: owner_active
                .then(|| "owner lock is active without a protected control record".to_owned()),
        });
    };
    let owner_active = runtime_owner_is_active(context)?;
    if !owner_active {
        let listener_active = listener_is_active(context.listen_address);
        return Ok(ClassifiedRuntime {
            classification: if listener_active {
                RuntimeClassification::ForeignListener
            } else {
                RuntimeClassification::Stale
            },
            response: None,
            reason: Some(
                "protected control state exists without the exact active owner lock".to_owned(),
            ),
        });
    }
    match authenticated_control(
        context,
        &record,
        ControlOperation::Status,
        Instant::now() + CONTROL_TIMEOUT,
    ) {
        Ok((_, mut response, _)) if response.accepted => {
            let mut classification = match response.state {
                SupervisorState::Starting => RuntimeClassification::Starting,
                SupervisorState::Ready => RuntimeClassification::Owned,
                SupervisorState::Stopping => RuntimeClassification::Stopping,
                SupervisorState::Failed => RuntimeClassification::Stale,
            };
            let mut reason = response.reason.clone();
            if classification == RuntimeClassification::Owned {
                let expected_pid = response
                    .workbench_pid
                    .ok_or_else(|| runtime_unavailable("owned response lacks workbench PID"))?;
                if let Ok(identity) =
                    probe_runtime_identity(context, record.capability(), expected_pid)
                {
                    // Application identity is live engagement authority and
                    // deliberately changes as governed state advances. The
                    // authenticated endpoint, not the immutable session
                    // record's readiness snapshot, is current authority.
                    response.application_identity_sha256 = Some(identity);
                } else {
                    classification = RuntimeClassification::Starting;
                    reason =
                        Some("authenticated application identity is not yet replayable".to_owned());
                }
            }
            Ok(ClassifiedRuntime {
                classification,
                response: Some(response),
                reason,
            })
        }
        Ok((_, response, _)) => Ok(ClassifiedRuntime {
            classification: RuntimeClassification::Stale,
            reason: response
                .reason
                .clone()
                .or_else(|| Some("authenticated supervisor rejected status replay".to_owned())),
            response: Some(response),
        }),
        Err(error) => Ok(ClassifiedRuntime {
            classification: if listener_is_active(context.listen_address) {
                RuntimeClassification::ForeignListener
            } else if owner_active {
                RuntimeClassification::Starting
            } else {
                RuntimeClassification::Stale
            },
            response: None,
            reason: Some(error.to_string()),
        }),
    }
}

fn report_from_classification(
    context: &RuntimeContext,
    classified: ClassifiedRuntime,
) -> RuntimeStatusReport {
    let response = classified.response.as_ref();
    RuntimeStatusReport {
        schema_version: RUNTIME_STATUS_SCHEMA,
        classification: classified.classification,
        bundle: context.bundle.clone(),
        workspace: context.workspace.clone(),
        listen_address: context.listen_address.to_string(),
        url: context.url(),
        runtime_root: context.runtime_root_path(),
        session_directory: context.session_path(),
        supervisor_pid: response.map(|response| response.supervisor_pid),
        workbench_pid: response.and_then(|response| response.workbench_pid),
        workbench_process_group_id: None,
        log_path: None,
        application_identity_sha256: response
            .and_then(|response| response.application_identity_sha256.clone()),
        reason: classified
            .reason
            .or_else(|| response.and_then(|response| response.reason.clone())),
    }
}

fn listener_is_active(address: SocketAddrV4) -> bool {
    TcpStream::connect_timeout(&SocketAddr::V4(address), LISTENER_PROBE_TIMEOUT).is_ok()
}

/// Verify and start or idempotently report one exact Windows runtime.
///
/// # Errors
///
/// Returns an error when the fixed bundle, workspace, private control state,
/// listener ownership, or authenticated readiness proof cannot be verified.
pub async fn start_runtime(options: &RuntimeCommandOptions) -> Result<RuntimeStatusReport> {
    let context = resolve_context(options)?;
    let _command_lock = lock_command(&context)?;
    start_runtime_locked(&context).await
}

// Startup publication, adoption, readiness and rollback form one security
// boundary. Retain the linear orchestration for auditability.
#[allow(clippy::too_many_lines)]
async fn start_runtime_locked(context: &RuntimeContext) -> Result<RuntimeStatusReport> {
    let classified = classify_runtime(context)?;
    match classified.classification {
        RuntimeClassification::Owned | RuntimeClassification::Starting => {
            return Ok(report_from_classification(context, classified));
        }
        RuntimeClassification::ForeignListener => {
            return Err(runtime_conflict(
                "the requested loopback port or control endpoint is foreign",
            ));
        }
        RuntimeClassification::Stopping => {
            return Err(runtime_conflict(
                "the exact owned runtime is still stopping",
            ));
        }
        RuntimeClassification::Stale => cleanup_stale_state(context)?,
        RuntimeClassification::Stopped => {}
    }
    if runtime_owner_is_active(context)? || listener_is_active(context.listen_address) {
        return Err(runtime_conflict(
            "runtime authority changed while preparing startup",
        ));
    }

    let capability = CapabilityToken::generate()
        .map_err(|error| native_error("runtime capability generation", error))?;
    let nonce = SessionNonce::generate()
        .map_err(|error| native_error("runtime session nonce generation", error))?;
    let issued = now_unix_millis()?;
    let lease_millis = u64::try_from(ADOPTION_LEASE.as_millis())
        .map_err(|_| runtime_unavailable("adoption lease exceeds protocol bounds"))?;
    let adoption_deadline = issued
        .checked_add(lease_millis)
        .ok_or_else(|| runtime_unavailable("adoption deadline overflows"))?;
    let request = LaunchRequest {
        schema_version: WINDOWS_LAUNCH_REQUEST_SCHEMA.to_owned(),
        capability: capability.lowercase_hex(),
        session_nonce: nonce.lowercase_hex(),
        session_key: context.session_key.clone(),
        pipe_component: context.pipe_component.clone(),
        bundle_root: context.bundle_root.path().to_path_buf(),
        bundle_manifest_sha256: context.bundle.manifest_sha256.clone(),
        bundle_commit: context.bundle.product_commit.clone(),
        workspace: stable_workspace_identity(&context.workspace),
        listen_address: context.listen_address.to_string(),
        url: context.url(),
        runtime_root: context.runtime_root_path(),
        session_directory: context.session_path(),
        controller_sha256: context.bundle.runtime_controller_sha256.clone(),
        workbench_sha256: context.bundle.workbench_sha256.clone(),
        issued_unix_millis: issued,
        adoption_deadline_unix_millis: adoption_deadline,
    };
    let request_bytes = canonical_bytes(&request)?;
    context
        .session
        .create_state_file_new(LAUNCH_REQUEST_COMPONENT, &request_bytes)
        .map_err(|error| native_error("publish Windows launch request", error))?;
    let prepared = (|| {
        context
            .system_root
            .verify()
            .map_err(|error| native_error("SystemRoot replay", error))?;
        let binding = authenticated_control_binding(&context.pipe_component, &capability)
            .map_err(|error| native_error("control binding", error))?;
        let arguments = vec![
            OsString::from("__supervise"),
            OsString::from("--request"),
            context.launch_request_path().into_os_string(),
        ];
        Ok((binding, arguments, minimal_environment(context)))
    })();
    let (binding, arguments, environment) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let cleanup = delete_published_launch(context, &request_bytes);
            return combine_start_cleanup(error, cleanup);
        }
    };
    let protected = ProtectedRecord::Launch { record: request };
    let mut supervisor = match OwnedProcess::launch(
        Rc::clone(&context.controller),
        &arguments,
        &environment,
        context.session.directory(),
        binding,
        nonce.clone(),
    ) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            return combine_start_cleanup(
                native_error("launch Windows supervisor", error),
                delete_published_launch(context, &request_bytes),
            );
        }
    };
    let supervisor_pid = match supervisor.process_id() {
        Ok(process_id) => process_id,
        Err(error) => {
            let error = native_error("supervisor process identity", error);
            let cleanup = cleanup_failed_start(context, &mut supervisor);
            return combine_start_cleanup(error, cleanup);
        }
    };
    let startup =
        adopt_and_wait_ready(context, &protected, &nonce, supervisor_pid, &supervisor).await;
    let completion = match startup {
        Ok((report, permit)) => (|| {
            supervisor
                .detach_after_authenticated_readiness(permit)
                .map_err(|error| native_error("detach authenticated supervisor", error))?;
            Ok(report)
        })(),
        Err(error) => Err(error),
    };
    match completion {
        Ok(report) => Ok(report),
        Err(error) => {
            let cleanup = cleanup_failed_start(context, &mut supervisor);
            match cleanup {
                Ok(()) => Err(error),
                Err(cleanup) => Err(runtime_unavailable(format!(
                    "{error}; failed-start cleanup was not proven: {cleanup}"
                ))),
            }
        }
    }
}

// The paired adoption/readiness polling loops intentionally share exact
// authority and deadline handling rather than splitting stateful checks.
#[allow(clippy::too_many_lines)]
async fn adopt_and_wait_ready(
    context: &RuntimeContext,
    record: &ProtectedRecord,
    nonce: &SessionNonce,
    supervisor_pid: u32,
    supervisor: &OwnedProcess,
) -> Result<(RuntimeStatusReport, AuthenticatedReadinessPermit)> {
    let adoption_deadline = Instant::now() + ADOPTION_LEASE;
    loop {
        if let Some(exit_code) = supervisor
            .try_wait()
            .map_err(|error| native_error("supervisor startup wait", error))?
        {
            return Err(runtime_unavailable(format!(
                "supervisor exited with {exit_code} before adoption"
            )));
        }
        match authenticated_control(
            context,
            record,
            ControlOperation::Adopt,
            Instant::now() + CONTROL_TIMEOUT,
        ) {
            Ok((_, response, _))
                if response.accepted
                    && response.state == SupervisorState::Starting
                    && response.supervisor_pid == supervisor_pid =>
            {
                break;
            }
            Ok(_) | Err(_) if Instant::now() < adoption_deadline => {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            Ok(_) | Err(_) => {
                return Err(runtime_unavailable(
                    "supervisor did not authenticate adoption before its lease expired",
                ));
            }
        }
    }

    let readiness_deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some(exit_code) = supervisor
            .try_wait()
            .map_err(|error| native_error("supervisor readiness wait", error))?
        {
            return Err(runtime_unavailable(format!(
                "supervisor exited with {exit_code} before readiness"
            )));
        }
        match authenticated_control(
            context,
            record,
            ControlOperation::Status,
            Instant::now() + CONTROL_TIMEOUT,
        ) {
            Ok((_, response, _))
                if response.accepted
                    && response.state == SupervisorState::Ready
                    && response.supervisor_pid == supervisor_pid =>
            {
                let initial_workbench_pid = response
                    .workbench_pid
                    .ok_or_else(|| runtime_unavailable("ready response lacks workbench PID"))?;
                verify_bundle_replay(
                    &context.bundle,
                    &context.bundle_root,
                    &context.entrypoint,
                    &context.launcher,
                    &context.controller,
                    &context.workbench,
                    &context.integrity_cli,
                )?;
                if Instant::now() >= readiness_deadline {
                    return Err(runtime_unavailable(
                        "bundle replay exhausted the readiness deadline",
                    ));
                }
                let (native_response, final_response, _) = authenticated_control(
                    context,
                    record,
                    ControlOperation::Status,
                    (Instant::now() + CONTROL_TIMEOUT).min(readiness_deadline),
                )?;
                let final_workbench_pid = final_response.workbench_pid.ok_or_else(|| {
                    runtime_unavailable("final ready response lacks workbench PID")
                })?;
                if !final_response.accepted
                    || final_response.state != SupervisorState::Ready
                    || final_response.supervisor_pid != supervisor_pid
                    || final_workbench_pid != initial_workbench_pid
                {
                    return Err(runtime_unavailable(
                        "runtime authority changed during final bundle replay",
                    ));
                }
                let current_identity =
                    probe_runtime_identity(context, record.capability(), final_workbench_pid)?;
                if final_response.application_identity_sha256.as_deref()
                    != Some(current_identity.as_str())
                {
                    return Err(runtime_unavailable(
                        "final application identity differs from the authenticated supervisor",
                    ));
                }
                let permit = readiness_permit(
                    context,
                    record,
                    native_response,
                    supervisor_pid,
                    nonce.clone(),
                )?;
                let application_still_current = matches!(
                    probe_runtime_identity(context, record.capability(), final_workbench_pid),
                    Ok(identity) if identity == current_identity
                );
                if supervisor
                    .try_wait()
                    .map_err(|error| native_error("post-readiness supervisor wait", error))?
                    .is_some()
                    || !application_still_current
                {
                    return Err(runtime_unavailable(
                        "runtime identity changed before supervisor adoption completed",
                    ));
                }
                return Ok((
                    report_from_classification(
                        context,
                        ClassifiedRuntime {
                            classification: RuntimeClassification::Owned,
                            response: Some(final_response),
                            reason: None,
                        },
                    ),
                    permit,
                ));
            }
            Ok((_, response, _)) if response.state == SupervisorState::Failed => {
                return Err(runtime_unavailable(
                    response
                        .reason
                        .unwrap_or_else(|| "workbench startup failed".to_owned()),
                ));
            }
            Ok(_) | Err(_) if Instant::now() < readiness_deadline => {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            Ok(_) | Err(_) => {
                return Err(runtime_unavailable(
                    "runtime did not reach authenticated application readiness",
                ));
            }
        }
    }
}

fn readiness_permit(
    context: &RuntimeContext,
    record: &ProtectedRecord,
    response: AuthenticatedPipeResponse,
    supervisor_pid: u32,
    nonce: SessionNonce,
) -> Result<AuthenticatedReadinessPermit> {
    response
        .validate_readiness(supervisor_pid, nonce, |bytes| {
            let candidate: ControlResponse = parse_canonical(bytes, "readiness response")
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            validate_control_response(context, record, &candidate)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            if !candidate.accepted
                || candidate.state != SupervisorState::Ready
                || candidate.supervisor_pid != supervisor_pid
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "readiness response is not exact ready supervisor evidence",
                ));
            }
            Ok(())
        })
        .map_err(|error| native_error("authenticated readiness permit", error))
}

fn minimal_environment(context: &RuntimeContext) -> Vec<(OsString, OsString)> {
    let temporary = context
        .session_temp
        .directory()
        .path()
        .as_os_str()
        .to_owned();
    vec![
        (
            OsString::from("SystemRoot"),
            context
                .system_root
                .environment_path()
                .as_os_str()
                .to_owned(),
        ),
        (OsString::from("TEMP"), temporary.clone()),
        (OsString::from("TMP"), temporary.clone()),
        (OsString::from("TMPDIR"), temporary.clone()),
        (OsString::from("SQLITE_TMPDIR"), temporary),
    ]
}

fn cleanup_failed_start(context: &RuntimeContext, supervisor: &mut OwnedProcess) -> Result<()> {
    supervisor
        .terminate_and_reap(1, Instant::now() + STOP_TIMEOUT)
        .map_err(|error| native_error("failed-start supervisor reap", error))?;
    let deadline = Instant::now() + STOP_TIMEOUT;
    while runtime_owner_is_active(context)? && Instant::now() < deadline {
        std::thread::sleep(POLL_INTERVAL);
    }
    if runtime_owner_is_active(context)? {
        return Err(runtime_unavailable(
            "failed-start supervisor owner lock remained active",
        ));
    }
    cleanup_stale_state(context)?;
    if listener_is_active(context.listen_address) {
        return Err(runtime_conflict(
            "failed-start cleanup preserved an unowned loopback listener",
        ));
    }
    Ok(())
}

fn delete_published_launch(context: &RuntimeContext, request_bytes: &[u8]) -> Result<()> {
    context
        .session
        .delete_state_file_exact(LAUNCH_REQUEST_COMPONENT, request_bytes)
        .map_err(|error| native_error("rollback exact Windows launch request", error))
}

fn combine_start_cleanup<T>(error: KernelError, cleanup: Result<()>) -> Result<T> {
    match cleanup {
        Ok(()) => Err(error),
        Err(cleanup) => Err(runtime_unavailable(format!(
            "{error}; startup rollback was not proven: {cleanup}"
        ))),
    }
}

fn cleanup_stale_state(context: &RuntimeContext) -> Result<()> {
    if runtime_owner_is_active(context)? || listener_is_active(context.listen_address) {
        return Err(runtime_conflict(
            "stale-state cleanup refused active owner or listener authority",
        ));
    }
    let (launch, session) = load_protected_record_pair(context)?;
    if let Some((_, bytes)) = session {
        context
            .session
            .delete_state_file_exact(SESSION_RECORD_COMPONENT, &bytes)
            .map_err(|error| native_error("delete exact stale session record", error))?;
    }
    if let Some((_, bytes)) = launch {
        context
            .session
            .delete_state_file_exact(LAUNCH_REQUEST_COMPONENT, &bytes)
            .map_err(|error| native_error("delete exact stale launch request", error))?;
    }
    Ok(())
}

/// Verify and classify one exact runtime without signalling a process.
///
/// # Errors
///
/// Returns an error when the fixed bundle, workspace, private control state,
/// or runtime ownership evidence cannot be replayed.
pub fn status_runtime(options: &RuntimeCommandOptions) -> Result<RuntimeStatusReport> {
    let context = resolve_context(options)?;
    let _command_lock = lock_command(&context)?;
    let classified = classify_runtime(&context)?;
    Ok(report_from_classification(&context, classified))
}

/// Execute the disposable native Windows supervisor.
///
/// # Errors
///
/// Returns an error when launch authority, the authenticated pipe, the
/// Job-owned workbench, or required cleanup cannot be proven.
pub async fn run_supervisor(request_path: &Path) -> Result<()> {
    let (context, request, launch_bytes) = resolve_supervisor_context(request_path)?;
    validate_launch_request(&context, &request, true)?;
    let _owner_lock = context
        .session
        .open_and_lock(OWNER_LOCK_COMPONENT)
        .map_err(|error| native_error("runtime supervisor owner lock", error))?;
    let capability = CapabilityToken::parse_lowercase_hex(&request.capability)
        .map_err(|error| native_error("supervisor capability", error))?;
    let mut server = AuthenticatedPipeServer::create(
        Rc::clone(&context.user),
        &context.pipe_component,
        capability,
    )
    .map_err(|error| native_error("private control pipe", error))?;
    wait_for_adoption(&context, &request, &mut server)?;
    supervise_workbench(&context, &request, &launch_bytes, &mut server)
}

fn resolve_supervisor_context(
    request_path: &Path,
) -> Result<(RuntimeContext, LaunchRequest, Vec<u8>)> {
    if !request_path.is_absolute()
        || request_path.file_name() != Some(OsStr::new(LAUNCH_REQUEST_COMPONENT))
    {
        return Err(runtime_unavailable(
            "supervisor launch request path is not the fixed private component",
        ));
    }
    let session_path = request_path
        .parent()
        .ok_or_else(|| runtime_unavailable("supervisor request has no session parent"))?;
    let session_component = session_path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| runtime_unavailable("supervisor session component is invalid"))?;
    let user = CurrentUser::capture().map_err(|error| native_error("current user SID", error))?;
    let local = local_app_data_directory()
        .map_err(|error| native_error("LocalAppData authority", error))?;
    let runtime_root = PrivateRuntimeDirectory::open_or_create_child(
        local,
        Rc::clone(&user),
        RUNTIME_ROOT_COMPONENT,
    )
    .map_err(|error| native_error("private runtime root", error))?;
    let session = PrivateRuntimeDirectory::open_or_create_child(
        runtime_root.directory(),
        user,
        session_component,
    )
    .map_err(|error| native_error("private runtime session", error))?;
    let supplied_session = RealDirectory::capture(session_path)
        .map_err(|error| native_error("supplied session identity", error))?;
    if supplied_session.identity() != session.directory().identity() {
        return Err(runtime_unavailable(
            "supervisor request escaped the exact private session",
        ));
    }
    let bytes = session
        .read_state_file(LAUNCH_REQUEST_COMPONENT, MAXIMUM_STATE_BYTES)
        .map_err(|error| native_error("protected supervisor launch request", error))?;
    let request: LaunchRequest = parse_canonical(&bytes, "Windows launch request")?;
    let listen = parse_listen(&request.listen_address)?;
    let context = resolve_context(&RuntimeCommandOptions {
        workspace: request.workspace.canonical_path.clone(),
        port: listen.port(),
    })?;
    if context.session.directory().identity() != supplied_session.identity()
        || context.session_path() != session.directory().path()
    {
        return Err(runtime_unavailable(
            "derived supervisor session identity differs",
        ));
    }
    Ok((context, request, bytes))
}

fn wait_for_adoption(
    context: &RuntimeContext,
    request: &LaunchRequest,
    server: &mut AuthenticatedPipeServer,
) -> Result<()> {
    let now = now_unix_millis()?;
    let remaining_millis = request
        .adoption_deadline_unix_millis
        .checked_sub(now)
        .ok_or_else(|| runtime_unavailable("supervisor adoption lease already expired"))?;
    let deadline = Instant::now() + Duration::from_millis(remaining_millis);
    loop {
        if Instant::now() >= deadline {
            return Err(runtime_unavailable(
                "supervisor exited because authenticated adoption did not occur",
            ));
        }
        let transaction = match server.accept((Instant::now() + CONTROL_TIMEOUT).min(deadline)) {
            Ok(transaction) => transaction,
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut && Instant::now() < deadline =>
            {
                continue;
            }
            Err(error) => return Err(native_error("supervisor adoption accept", error)),
        };
        let parsed =
            parse_canonical::<ControlRequest>(transaction.payload(), "Windows adoption request");
        let accepted = parsed.as_ref().is_ok_and(|control| {
            control.schema_version == WINDOWS_CONTROL_REQUEST_SCHEMA
                && control.operation == ControlOperation::Adopt
                && control.session_key == context.session_key
                && control.session_nonce == request.session_nonce
        });
        let response = supervisor_response(
            context,
            request,
            SupervisorState::Starting,
            None,
            None,
            accepted,
            (!accepted).then(|| "control request did not adopt this exact session".to_owned()),
        );
        let response_bytes = canonical_bytes(&response)?;
        transaction
            .finish(&response_bytes)
            .map_err(|error| native_error("supervisor adoption response", error))?;
        if accepted {
            return Ok(());
        }
    }
}

fn supervise_workbench(
    context: &RuntimeContext,
    request: &LaunchRequest,
    launch_bytes: &[u8],
    server: &mut AuthenticatedPipeServer,
) -> Result<()> {
    context
        .system_root
        .verify()
        .map_err(|error| native_error("SystemRoot replay", error))?;
    let job = KillOnCloseJob::new().map_err(|error| native_error("workbench Job Object", error))?;
    let arguments = workbench_arguments(context, request);
    let environment = minimal_environment(context);
    let child = job
        .spawn_and_assign(
            Rc::clone(&context.workbench),
            &arguments,
            &environment,
            context.session.directory(),
        )
        .map_err(|error| native_error("Job-owned workbench launch", error))?;
    let workbench_pid = child.process_id();
    let mut session_bytes = None;
    let result = supervise_workbench_loop(
        context,
        request,
        server,
        &job,
        &child,
        workbench_pid,
        &mut session_bytes,
    );

    let termination = if result.is_err() {
        match child.try_wait() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => terminate_job_and_reap(&job, &child),
            Err(error) => Err(native_error("failed supervisor child poll", error)),
        }
    } else {
        Ok(())
    };
    let result = match (result, termination) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) | (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => Err(runtime_unavailable(format!(
            "{error}; Job termination/reap failed: {cleanup}"
        ))),
    };
    let cleanup = cleanup_supervisor_state(context, launch_bytes, session_bytes.as_deref());
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Ok(()), Err(error)) | (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => Err(runtime_unavailable(format!(
            "{error}; supervisor state cleanup failed: {cleanup}"
        ))),
    }
}

// Control handling, state publication and Job cleanup must retain one ordered
// supervisor loop; extracting fragments would obscure its authority boundary.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn supervise_workbench_loop(
    context: &RuntimeContext,
    request: &LaunchRequest,
    server: &mut AuthenticatedPipeServer,
    job: &KillOnCloseJob,
    child: &ManagedProcess,
    workbench_pid: u32,
    session_bytes: &mut Option<Vec<u8>>,
) -> Result<()> {
    let startup_deadline = Instant::now() + START_TIMEOUT;
    let mut application_identity = None;
    loop {
        if let Some(exit_code) = child
            .try_wait()
            .map_err(|error| native_error("workbench wait", error))?
        {
            return Err(runtime_unavailable(format!(
                "workbench exited with {exit_code}"
            )));
        }
        if session_bytes.is_none() {
            if let Ok(identity) =
                probe_runtime_identity(context, &request.capability, workbench_pid)
            {
                let record = SessionRecord {
                    schema_version: WINDOWS_SESSION_RECORD_SCHEMA.to_owned(),
                    capability: request.capability.clone(),
                    session_nonce: request.session_nonce.clone(),
                    session_key: context.session_key.clone(),
                    pipe_component: context.pipe_component.clone(),
                    bundle_root: context.bundle_root.path().to_path_buf(),
                    bundle_manifest_sha256: context.bundle.manifest_sha256.clone(),
                    bundle_commit: context.bundle.product_commit.clone(),
                    workspace: stable_workspace_identity(&context.workspace),
                    listen_address: context.listen_address.to_string(),
                    url: context.url(),
                    supervisor_pid: std::process::id(),
                    workbench_pid,
                    workbench_sha256: context.bundle.workbench_sha256.clone(),
                    application_identity_sha256: identity.clone(),
                };
                let bytes = canonical_bytes(&record)?;
                context
                    .session
                    .create_state_file_new(SESSION_RECORD_COMPONENT, &bytes)
                    .map_err(|error| native_error("publish Windows session record", error))?;
                application_identity = Some(identity);
                *session_bytes = Some(bytes);
            } else if Instant::now() >= startup_deadline {
                return Err(runtime_unavailable(
                    "workbench did not expose authenticated application readiness",
                ));
            }
        } else {
            // Application identity is mutable governed engagement authority.
            // Refresh it for every control response instead of treating the
            // immutable readiness record as ongoing authority.
            application_identity =
                probe_runtime_identity(context, &request.capability, workbench_pid).ok();
        }

        let transaction = match server.accept(Instant::now() + CONTROL_TIMEOUT) {
            Ok(transaction) => transaction,
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => continue,
            Err(error) => return Err(native_error("authenticated control accept", error)),
        };
        let Ok(control) =
            parse_canonical::<ControlRequest>(transaction.payload(), "Windows control request")
        else {
            let response = supervisor_response(
                context,
                request,
                current_supervisor_state(application_identity.as_deref()),
                Some(workbench_pid),
                application_identity.clone(),
                false,
                Some("control request was not canonical protocol v2".to_owned()),
            );
            transaction
                .finish(&canonical_bytes(&response)?)
                .map_err(|error| native_error("rejected control response", error))?;
            continue;
        };
        let authenticated = control.schema_version == WINDOWS_CONTROL_REQUEST_SCHEMA
            && control.session_key == context.session_key
            && control.session_nonce == request.session_nonce;
        let operation_allowed = authenticated && control.operation != ControlOperation::Adopt;
        let stop = operation_allowed && control.operation == ControlOperation::Stop;
        let state = if stop {
            SupervisorState::Stopping
        } else {
            current_supervisor_state(application_identity.as_deref())
        };
        let response = supervisor_response(
            context,
            request,
            state,
            Some(workbench_pid),
            application_identity.clone(),
            operation_allowed,
            (!operation_allowed).then(|| "control request did not match this session".to_owned()),
        );
        transaction
            .finish(&canonical_bytes(&response)?)
            .map_err(|error| native_error("authenticated control response", error))?;
        if stop {
            return terminate_job_and_reap(job, child);
        }
    }
}

fn current_supervisor_state(application_identity: Option<&str>) -> SupervisorState {
    if application_identity.is_some() {
        SupervisorState::Ready
    } else {
        SupervisorState::Starting
    }
}

fn workbench_arguments(context: &RuntimeContext, request: &LaunchRequest) -> Vec<OsString> {
    let root = context.bundle_root.path();
    vec![
        OsString::from("--workspace"),
        request.workspace.canonical_path.as_os_str().to_owned(),
        OsString::from("--engagement-setup"),
        OsString::from("--engagement-intake-policy"),
        root.join("config/engagement-intake-policy.json")
            .into_os_string(),
        OsString::from("--engagement-profiling-policy"),
        root.join("config/engagement-profiling-policy.json")
            .into_os_string(),
        OsString::from("--engagement-validation-policy"),
        root.join("config/engagement-validation-policy.json")
            .into_os_string(),
        OsString::from("--object-packs"),
        root.join("object-packs").into_os_string(),
        OsString::from("--profile-schema"),
        root.join("schemas/migration-object-profile.schema.json")
            .into_os_string(),
        OsString::from("--mapping-schema"),
        root.join("schemas/source-mapping-version.schema.json")
            .into_os_string(),
        OsString::from("--assets"),
        root.join("web").into_os_string(),
        OsString::from("--listen"),
        OsString::from(&request.listen_address),
        OsString::from("--runtime-session-token"),
        OsString::from(&request.capability),
    ]
}

fn supervisor_response(
    context: &RuntimeContext,
    request: &LaunchRequest,
    state: SupervisorState,
    workbench_pid: Option<u32>,
    application_identity_sha256: Option<String>,
    accepted: bool,
    reason: Option<String>,
) -> ControlResponse {
    ControlResponse {
        schema_version: WINDOWS_CONTROL_RESPONSE_SCHEMA.to_owned(),
        accepted,
        state,
        session_key: context.session_key.clone(),
        session_nonce: request.session_nonce.clone(),
        bundle_root: context.bundle_root.path().to_path_buf(),
        bundle_manifest_sha256: context.bundle.manifest_sha256.clone(),
        bundle_commit: context.bundle.product_commit.clone(),
        workspace: stable_workspace_identity(&context.workspace),
        listen_address: context.listen_address.to_string(),
        url: context.url(),
        supervisor_pid: std::process::id(),
        workbench_pid,
        workbench_sha256: context.bundle.workbench_sha256.clone(),
        application_identity_sha256,
        reason,
    }
}

fn terminate_job_and_reap(job: &KillOnCloseJob, child: &ManagedProcess) -> Result<()> {
    if child
        .try_wait()
        .map_err(|error| native_error("workbench termination poll", error))?
        .is_none()
    {
        job.terminate(0)
            .map_err(|error| native_error("terminate workbench Job Object", error))?;
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        if child
            .try_wait()
            .map_err(|error| native_error("workbench reap", error))?
            .is_some()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            // This process is the disposable fail-stop boundary. Aborting
            // closes the kill-on-close Job and cannot leave this tree adopted.
            std::process::abort();
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn cleanup_supervisor_state(
    context: &RuntimeContext,
    launch_bytes: &[u8],
    session_bytes: Option<&[u8]>,
) -> Result<()> {
    let deadline = Instant::now() + STOP_TIMEOUT;
    while listener_is_active(context.listen_address) && Instant::now() < deadline {
        std::thread::sleep(POLL_INTERVAL);
    }
    if listener_is_active(context.listen_address) {
        return Err(runtime_unavailable(
            "Job-owned workbench listener did not become quiet",
        ));
    }
    context
        .session
        .delete_state_file_exact(LAUNCH_REQUEST_COMPONENT, launch_bytes)
        .map_err(|error| native_error("delete exact launch request", error))?;
    if let Some(bytes) = session_bytes {
        context
            .session
            .delete_state_file_exact(SESSION_RECORD_COMPONENT, bytes)
            .map_err(|error| native_error("delete exact session record", error))?;
    }
    Ok(())
}

fn probe_runtime_identity(
    context: &RuntimeContext,
    session_token: &str,
    expected_workbench_pid: u32,
) -> Result<String> {
    let mut stream =
        TcpStream::connect_timeout(&SocketAddr::V4(context.listen_address), CONTROL_TIMEOUT)
            .map_err(|error| {
                runtime_unavailable(format!("application identity connection: {error}"))
            })?;
    stream
        .set_read_timeout(Some(CONTROL_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(CONTROL_TIMEOUT)))
        .map_err(|error| runtime_unavailable(format!("application identity timeout: {error}")))?;
    let request = format!(
        "GET /__migration-stream/runtime-identity HTTP/1.1\r\nHost: {}\r\nX-Migration-Stream-Runtime-Token: {}\r\nConnection: close\r\nAccept: application/json\r\n\r\n",
        context.listen_address, session_token
    );
    stream.write_all(request.as_bytes()).map_err(|error| {
        runtime_unavailable(format!("application identity request failed: {error}"))
    })?;
    // Keep the request side open until the HTTP/1.1 server has delivered its
    // response. A premature write-side shutdown is observable by Hyper as a
    // disconnected client and can suppress the otherwise valid response.
    // `Connection: close` and the retained read timeout bound completion.
    let response_bound = u64::try_from(MAXIMUM_STATE_BYTES + 16 * 1024)
        .map_err(|_| runtime_unavailable("application response bound exceeds u64"))?;
    let mut response = Vec::new();
    (&mut stream)
        .take(response_bound + 1)
        .read_to_end(&mut response)
        .map_err(|error| {
            runtime_unavailable(format!("application identity response failed: {error}"))
        })?;
    if u64::try_from(response.len()).unwrap_or(u64::MAX) > response_bound {
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
    let value = parse_unique_json(&response[header_end..]).map_err(|error| {
        runtime_unavailable(format!("application identity JSON is invalid: {error}"))
    })?;
    if canonical_json_bytes(&value)? != response[header_end..] {
        return Err(runtime_unavailable(
            "application identity JSON is not canonical",
        ));
    }
    let identity: RuntimeHttpIdentity = serde_json::from_value(value).map_err(|error| {
        runtime_unavailable(format!("application identity contract is invalid: {error}"))
    })?;
    let token_hash = format!("{:x}", Sha256::digest(session_token.as_bytes()));
    if identity.schema_version != APPLICATION_IDENTITY_SCHEMA
        || identity.session_token_sha256 != token_hash
        || identity.process_id != expected_workbench_pid
        || identity.listen_address != context.listen_address.to_string()
        || identity.workspace_path != context.workspace.canonical_path
        || identity.filesystem_identity_sha256 != context.workspace.filesystem_identity_sha256
        || !is_lowercase_sha256(&identity.application_identity_sha256)
    {
        return Err(runtime_unavailable(
            "application identity does not match the exact owned runtime",
        ));
    }
    Ok(identity.application_identity_sha256)
}

fn parse_listen(value: &str) -> Result<SocketAddrV4> {
    let parsed: SocketAddrV4 = value
        .parse()
        .map_err(|_| runtime_unavailable("runtime listen address is invalid"))?;
    if *parsed.ip() != Ipv4Addr::LOCALHOST || parsed.port() == 0 {
        return Err(runtime_unavailable(
            "runtime listen address must be literal 127.0.0.1",
        ));
    }
    Ok(parsed)
}

/// Stop only an authenticated exact owned Windows supervisor.
///
/// # Errors
///
/// Returns an error when ownership cannot be authenticated, a foreign listener
/// is present, or the exact owned runtime cannot be stopped within its bound.
pub async fn stop_runtime(options: &RuntimeCommandOptions) -> Result<RuntimeStatusReport> {
    let context = resolve_context(options)?;
    let _command_lock = lock_command(&context)?;
    stop_runtime_locked(&context).await
}

async fn stop_runtime_locked(context: &RuntimeContext) -> Result<RuntimeStatusReport> {
    let classified = classify_runtime(context)?;
    match classified.classification {
        RuntimeClassification::Stopped => {}
        RuntimeClassification::Stale => cleanup_stale_state(context)?,
        RuntimeClassification::ForeignListener => {
            return Err(runtime_conflict(
                "the requested port has a foreign listener; nothing was signalled",
            ));
        }
        RuntimeClassification::Owned | RuntimeClassification::Starting => {
            let record = load_protected_record(context)?.ok_or_else(|| {
                runtime_conflict("active owner has no protected control authority")
            })?;
            let (_, response, _) = authenticated_control(
                context,
                &record,
                ControlOperation::Stop,
                Instant::now() + CONTROL_TIMEOUT,
            )?;
            if !response.accepted || response.state != SupervisorState::Stopping {
                return Err(runtime_conflict(
                    "authenticated supervisor did not accept stop",
                ));
            }
            wait_for_owned_stop(context).await?;
        }
        RuntimeClassification::Stopping => wait_for_owned_stop(context).await?,
    }
    if !runtime_owner_is_active(context)? && !listener_is_active(context.listen_address) {
        cleanup_stale_state(context)?;
    }
    let final_classification = classify_runtime(context)?;
    if final_classification.classification == RuntimeClassification::ForeignListener {
        return Err(runtime_conflict(
            "the owned runtime stopped, but a foreign listener now occupies the port",
        ));
    }
    Ok(report_from_classification(context, final_classification))
}

async fn wait_for_owned_stop(context: &RuntimeContext) -> Result<()> {
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        let owner_active = runtime_owner_is_active(context)?;
        let listener_active = listener_is_active(context.listen_address);
        if !owner_active && !listener_active {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(runtime_unavailable(
                "authenticated Windows runtime did not stop within its bound",
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Restart with independent workspace and application identity replay.
///
/// # Errors
///
/// Returns an error when stop/start or either identity replay fails, including
/// any inability to prove cleanup of a replacement runtime.
pub async fn restart_runtime(options: &RuntimeCommandOptions) -> Result<RuntimeStatusReport> {
    let context = resolve_context(options)?;
    let _command_lock = lock_command(&context)?;
    let before_workspace = context.workspace.clone();
    let classified = classify_runtime(&context)?;
    if classified.classification == RuntimeClassification::ForeignListener {
        return Err(runtime_conflict(
            "restart cannot replace or signal a foreign listener",
        ));
    }
    let before_application_identity = if classified.classification == RuntimeClassification::Owned {
        classified
            .response
            .as_ref()
            .and_then(|response| response.application_identity_sha256.clone())
    } else {
        None
    };
    if classified.classification != RuntimeClassification::Stopped {
        let _ = stop_runtime_locked(&context).await?;
    }
    let refreshed = resolve_context(options)?;
    let report = start_runtime_locked(&refreshed).await?;
    if report.workspace != before_workspace {
        return combine_restart_cleanup(
            runtime_unavailable("workspace identity changed across Windows restart"),
            stop_runtime_locked(&refreshed).await.map(|_| ()),
        );
    }
    if let Some(before) = before_application_identity
        && report.application_identity_sha256.as_deref() != Some(before.as_str())
    {
        return combine_restart_cleanup(
            runtime_unavailable("application identity changed across Windows restart"),
            stop_runtime_locked(&refreshed).await.map(|_| ()),
        );
    }
    Ok(report)
}

fn combine_restart_cleanup<T>(error: KernelError, cleanup: Result<()>) -> Result<T> {
    match cleanup {
        Ok(()) => Err(error),
        Err(cleanup) => Err(runtime_unavailable(format!(
            "{error}; replacement-runtime cleanup was not proven: {cleanup}"
        ))),
    }
}

/// Run read-only integrity through this exact verified controller while the
/// selected runtime is stopped. The bundled CLI member remains independently
/// verified and is reported as provenance; no shell or ambient PATH is used.
///
/// # Errors
///
/// Returns an error when the runtime is not safely stopped or bundle,
/// workspace, or integrity authority cannot be replayed.
pub fn integrity_runtime(options: &RuntimeCommandOptions) -> Result<RuntimeIntegrityReport> {
    let context = resolve_context(options)?;
    let _command_lock = lock_command(&context)?;
    let classified = classify_runtime(&context)?;
    match classified.classification {
        RuntimeClassification::Stopped => {}
        RuntimeClassification::Stale => cleanup_stale_state(&context)?,
        RuntimeClassification::Owned
        | RuntimeClassification::Starting
        | RuntimeClassification::Stopping
        | RuntimeClassification::ForeignListener => {
            return Err(runtime_conflict(
                "integrity requires the selected Windows runtime to be stopped",
            ));
        }
    }
    verify_bundle_replay(
        &context.bundle,
        &context.bundle_root,
        &context.entrypoint,
        &context.launcher,
        &context.controller,
        &context.workbench,
        &context.integrity_cli,
    )?;
    let integrity = integrity_check(&context.workspace.canonical_path)?;
    if !integrity.ok {
        return Err(KernelError::IntegrityFailed(integrity.issues.join("; ")));
    }
    let integrity_result = serde_json::to_value(integrity)?;
    let workspace_after = capture_workspace(&context.workspace.canonical_path)?;
    if workspace_after != context.workspace {
        return Err(runtime_unavailable(
            "workspace identity changed during Windows integrity replay",
        ));
    }
    verify_bundle_replay(
        &context.bundle,
        &context.bundle_root,
        &context.entrypoint,
        &context.launcher,
        &context.controller,
        &context.workbench,
        &context.integrity_cli,
    )?;
    Ok(RuntimeIntegrityReport {
        schema_version: RUNTIME_INTEGRITY_SCHEMA,
        bundle: context.bundle,
        workspace: context.workspace,
        bundled_cli_path: context.integrity_cli.path().to_path_buf(),
        integrity_result,
    })
}

fn capture_workspace(path: &Path) -> Result<RuntimeWorkspaceIdentity> {
    let directory = RealDirectory::capture(path)
        .map_err(|error| native_error("workspace identity replay", error))?;
    let status = ObjectWorkspaceService::new(directory.path())?.inspect()?;
    Ok(RuntimeWorkspaceIdentity {
        canonical_path: directory.path().to_path_buf(),
        filesystem_identity_sha256: windows_workspace_filesystem_identity_sha256(
            directory.identity(),
        ),
        object_workspace_state: status.state,
        object_workspace: status.workspace,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{
        ControlResponse, ProtectedRecord, SessionRecord, StableWorkspaceIdentity, SupervisorState,
        combine_restart_cleanup, response_process_ids_match, runtime_unavailable,
        stable_workspace_matches,
    };
    use crate::{
        object_workspace::{ObjectWorkspaceRecord, ObjectWorkspaceState},
        runtime_lifecycle::RuntimeWorkspaceIdentity,
    };

    fn record(workspace_id: &str, descriptor_hash: &str) -> ObjectWorkspaceRecord {
        ObjectWorkspaceRecord {
            schema_version: "migration-stream/object-workspace/v1".to_owned(),
            workspace_id: workspace_id.to_owned(),
            generation: 1,
            display_name: "Fixture".to_owned(),
            migration_object_name_or_scope: "Fixture".to_owned(),
            creator_attribution: "test".to_owned(),
            established_by: "INITIALIZED".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            descriptor_hash: descriptor_hash.to_owned(),
        }
    }

    fn runtime(record: Option<ObjectWorkspaceRecord>) -> RuntimeWorkspaceIdentity {
        RuntimeWorkspaceIdentity {
            canonical_path: PathBuf::from(r"C:\workspace"),
            filesystem_identity_sha256: "a".repeat(64),
            object_workspace_state: if record.is_some() {
                ObjectWorkspaceState::Active
            } else {
                ObjectWorkspaceState::InitializeRequired
            },
            object_workspace: record,
        }
    }

    fn stable_identity() -> StableWorkspaceIdentity {
        StableWorkspaceIdentity {
            canonical_path: PathBuf::from(r"C:\workspace"),
            filesystem_identity_sha256: "a".repeat(64),
            workspace_id: Some("workspace-a".to_owned()),
            descriptor_sha256: Some("b".repeat(64)),
        }
    }

    #[test]
    fn stable_workspace_binding_allows_only_monotonic_initialization() {
        let unbound = StableWorkspaceIdentity {
            canonical_path: PathBuf::from(r"C:\workspace"),
            filesystem_identity_sha256: "a".repeat(64),
            workspace_id: None,
            descriptor_sha256: None,
        };
        assert!(stable_workspace_matches(&unbound, &runtime(None)));
        assert!(stable_workspace_matches(
            &unbound,
            &runtime(Some(record("workspace-a", &"b".repeat(64))))
        ));

        let bound = StableWorkspaceIdentity {
            workspace_id: Some("workspace-a".to_owned()),
            descriptor_sha256: Some("b".repeat(64)),
            ..unbound
        };
        assert!(stable_workspace_matches(
            &bound,
            &runtime(Some(record("workspace-a", &"b".repeat(64))))
        ));
        assert!(!stable_workspace_matches(&bound, &runtime(None)));
        assert!(!stable_workspace_matches(
            &bound,
            &runtime(Some(record("workspace-b", &"b".repeat(64))))
        ));
        assert!(!stable_workspace_matches(
            &bound,
            &runtime(Some(record("workspace-a", &"c".repeat(64))))
        ));

        let id_only = StableWorkspaceIdentity {
            workspace_id: Some("workspace-a".to_owned()),
            descriptor_sha256: None,
            ..bound.clone()
        };
        assert!(!stable_workspace_matches(
            &id_only,
            &runtime(Some(record("workspace-a", &"b".repeat(64))))
        ));
        let descriptor_only = StableWorkspaceIdentity {
            workspace_id: None,
            descriptor_sha256: Some("b".repeat(64)),
            ..bound
        };
        assert!(!stable_workspace_matches(
            &descriptor_only,
            &runtime(Some(record("workspace-a", &"b".repeat(64))))
        ));
    }

    #[test]
    fn stable_workspace_always_binds_path_and_native_digest() {
        let stable = StableWorkspaceIdentity {
            canonical_path: PathBuf::from(r"C:\workspace"),
            filesystem_identity_sha256: "a".repeat(64),
            workspace_id: None,
            descriptor_sha256: None,
        };
        let mut moved = runtime(Some(record("workspace-a", &"b".repeat(64))));
        moved.canonical_path = PathBuf::from(r"C:\moved");
        assert!(!stable_workspace_matches(&stable, &moved));
        let mut replaced = runtime(Some(record("workspace-a", &"b".repeat(64))));
        replaced.filesystem_identity_sha256 = "c".repeat(64);
        assert!(!stable_workspace_matches(&stable, &replaced));
    }

    #[test]
    fn session_control_response_requires_exact_recorded_processes() {
        let record = ProtectedRecord::Session {
            record: SessionRecord {
                schema_version: super::WINDOWS_SESSION_RECORD_SCHEMA.to_owned(),
                capability: "1".repeat(64),
                session_nonce: "2".repeat(64),
                session_key: "3".repeat(64),
                pipe_component: "control-test".to_owned(),
                bundle_root: PathBuf::from(r"C:\bundle"),
                bundle_manifest_sha256: "4".repeat(64),
                bundle_commit: "5".repeat(40),
                workspace: stable_identity(),
                listen_address: "127.0.0.1:4173".to_owned(),
                url: "http://127.0.0.1:4173".to_owned(),
                supervisor_pid: 41,
                workbench_pid: 42,
                workbench_sha256: "6".repeat(64),
                application_identity_sha256: "7".repeat(64),
            },
        };
        let mut response = ControlResponse {
            schema_version: super::WINDOWS_CONTROL_RESPONSE_SCHEMA.to_owned(),
            accepted: true,
            state: SupervisorState::Ready,
            session_key: "3".repeat(64),
            session_nonce: "2".repeat(64),
            bundle_root: PathBuf::from(r"C:\bundle"),
            bundle_manifest_sha256: "4".repeat(64),
            bundle_commit: "5".repeat(40),
            workspace: stable_identity(),
            listen_address: "127.0.0.1:4173".to_owned(),
            url: "http://127.0.0.1:4173".to_owned(),
            supervisor_pid: 41,
            workbench_pid: Some(42),
            workbench_sha256: "6".repeat(64),
            application_identity_sha256: Some("7".repeat(64)),
            reason: None,
        };
        assert!(response_process_ids_match(&record, &response));
        response.supervisor_pid = 99;
        assert!(!response_process_ids_match(&record, &response));
        response.supervisor_pid = 41;
        response.workbench_pid = Some(99);
        assert!(!response_process_ids_match(&record, &response));
        response.workbench_pid = None;
        assert!(!response_process_ids_match(&record, &response));
    }

    #[test]
    fn restart_mismatch_reports_unproven_replacement_cleanup() {
        let error = combine_restart_cleanup::<()>(
            runtime_unavailable("post-start identity mismatch"),
            Err(runtime_unavailable("authenticated stop failed")),
        )
        .expect_err("cleanup failure must remain visible");
        let rendered = error.to_string();
        assert!(rendered.contains("post-start identity mismatch"));
        assert!(rendered.contains("replacement-runtime cleanup was not proven"));
        assert!(rendered.contains("authenticated stop failed"));
    }
}

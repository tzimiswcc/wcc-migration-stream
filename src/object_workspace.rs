//! Durable authority for one startup-selected migration-object workspace.
//!
//! The browser never chooses or receives a filesystem path. The local process
//! fixes one real folder at startup; this module binds that folder's immutable
//! human identity to both a small canonical descriptor and `SQLite`.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek as _, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use std::collections::HashMap;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, Transaction, params, types::ValueRef};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, canonical_json_string, parse_unique_json},
    durability::publish_new,
    state::{
        DATABASE_FILENAME, SCHEMA_VERSION, WorkspaceLock, WorkspaceWriter, integrity_check,
        open_read_only,
    },
};

pub const OBJECT_WORKSPACE_DESCRIPTOR_FILENAME: &str = "workspace.json";
pub const OBJECT_WORKSPACE_DESCRIPTOR_SCHEMA: &str = "migration-stream/object-workspace/v1";
pub const OBJECT_WORKSPACE_STATUS_SCHEMA: &str = "migration-stream/object-workspace/status/v1";
pub const INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA: &str =
    "migration-stream/object-workspace/write/initialize/v1";
pub const ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA: &str =
    "migration-stream/object-workspace/write/adopt/v1";

const MAXIMUM_DESCRIPTOR_BYTES: u64 = 16 * 1024;
const MAXIMUM_TEXT_BYTES: usize = 256;
const MINIMUM_IDEMPOTENCY_KEY_BYTES: usize = 16;
const MAXIMUM_IDEMPOTENCY_KEY_BYTES: usize = 128;
const MAXIMUM_ADOPTION_EVIDENCE_FILES: usize = 100_000;
const MAXIMUM_ADOPTION_EVIDENCE_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const PENDING_LIFECYCLE_EVIDENCE_SCHEMA: &str =
    "migration-stream/object-workspace/pending-evidence/v1";

#[cfg(test)]
static DESCRIPTOR_PUBLISH_FAILURE_AFTER_NAMESPACE: std::sync::Mutex<Option<PathBuf>> =
    std::sync::Mutex::new(None);
#[cfg(test)]
static FINAL_BINDING_EVIDENCE_MUTATION: std::sync::Mutex<Option<(PathBuf, PathBuf, Vec<u8>)>> =
    std::sync::Mutex::new(None);

const PENDING_NO_UPDATE_TRIGGER: &str =
    "CREATE TRIGGER object_workspace_pending_establishments_no_update
     BEFORE UPDATE ON object_workspace_pending_establishments
     BEGIN
         SELECT RAISE(ABORT, 'pending object workspace authority is immutable');
     END";
const PENDING_CONSUME_ONLY_TRIGGER: &str =
    "CREATE TRIGGER object_workspace_pending_establishments_consume_only
     BEFORE DELETE ON object_workspace_pending_establishments
     WHEN NOT EXISTS (
         SELECT 1
         FROM object_workspace_bindings AS binding
         WHERE binding.singleton = OLD.singleton
           AND binding.workspace_id = OLD.workspace_id
           AND binding.descriptor_hash = OLD.descriptor_hash
           AND binding.descriptor_json = OLD.descriptor_json
           AND binding.command_schema = OLD.command_schema
           AND binding.idempotency_key = OLD.idempotency_key
           AND binding.request_hash = OLD.request_hash
           AND binding.canonical_request_json = OLD.canonical_request_json
           AND (
               (OLD.action = 'INITIALIZE' AND binding.established_by = 'INITIALIZED')
               OR (OLD.action = 'ADOPT' AND binding.established_by = 'ADOPTED')
           )
     )
     BEGIN
         SELECT RAISE(ABORT, 'pending object workspace authority may only be consumed by its exact binding');
     END";
const BINDING_NO_UPDATE_TRIGGER: &str = "CREATE TRIGGER object_workspace_bindings_no_update
     BEFORE UPDATE ON object_workspace_bindings
     BEGIN
         SELECT RAISE(ABORT, 'object workspace binding is immutable');
     END";
const BINDING_NO_DELETE_TRIGGER: &str = "CREATE TRIGGER object_workspace_bindings_no_delete
     BEFORE DELETE ON object_workspace_bindings
     BEGIN
         SELECT RAISE(ABORT, 'object workspace binding is immutable');
     END";
const ENGAGEMENT_BINDING_NO_UPDATE_TRIGGER: &str =
    "CREATE TRIGGER object_workspace_engagement_bindings_no_update
     BEFORE UPDATE ON object_workspace_engagement_bindings
     BEGIN
         SELECT RAISE(ABORT, 'object workspace engagement binding is immutable');
     END";
const ENGAGEMENT_BINDING_NO_DELETE_TRIGGER: &str =
    "CREATE TRIGGER object_workspace_engagement_bindings_no_delete
     BEFORE DELETE ON object_workspace_engagement_bindings
     BEGIN
         SELECT RAISE(ABORT, 'object workspace engagement binding is immutable');
     END";

#[cfg(unix)]
fn make_owner_writable(permissions: &mut fs::Permissions) {
    use std::os::unix::fs::PermissionsExt as _;

    permissions.set_mode(permissions.mode() | 0o200);
}

#[cfg(not(unix))]
#[allow(
    clippy::permissions_set_readonly_false,
    reason = "this non-Unix branch clears only the platform read-only attribute"
)]
fn make_owner_writable(permissions: &mut fs::Permissions) {
    permissions.set_readonly(false);
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ObjectWorkspaceState {
    InitializeRequired,
    AdoptRequired,
    Active,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ObjectWorkspaceAction {
    Initialize,
    Adopt,
}

impl ObjectWorkspaceAction {
    const fn command_schema(self) -> &'static str {
        match self {
            Self::Initialize => INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA,
            Self::Adopt => ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA,
        }
    }

    const fn established_by(self) -> &'static str {
        match self {
            Self::Initialize => "INITIALIZED",
            Self::Adopt => "ADOPTED",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectWorkspaceDescriptor {
    pub schema_version: String,
    pub workspace_id: String,
    pub display_name: String,
    pub migration_object_name_or_scope: String,
    pub creator_attribution: String,
    pub established_by: String,
    pub created_at: String,
    pub command_schema: String,
    pub idempotency_key: String,
    pub request_hash: String,
    pub canonical_request_json: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectWorkspaceRecord {
    pub schema_version: String,
    pub workspace_id: String,
    pub generation: u64,
    pub display_name: String,
    pub migration_object_name_or_scope: String,
    pub creator_attribution: String,
    pub established_by: String,
    pub created_at: String,
    pub descriptor_hash: String,
}

impl ObjectWorkspaceRecord {
    fn from_descriptor(descriptor: &ObjectWorkspaceDescriptor, descriptor_hash: &str) -> Self {
        Self {
            schema_version: descriptor.schema_version.clone(),
            workspace_id: descriptor.workspace_id.clone(),
            generation: 1,
            display_name: descriptor.display_name.clone(),
            migration_object_name_or_scope: descriptor.migration_object_name_or_scope.clone(),
            creator_attribution: descriptor.creator_attribution.clone(),
            established_by: descriptor.established_by.clone(),
            created_at: descriptor.created_at.clone(),
            descriptor_hash: descriptor_hash.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectWorkspaceStatus {
    pub schema_version: String,
    pub state: ObjectWorkspaceState,
    pub state_hash: String,
    pub required_action: Option<ObjectWorkspaceAction>,
    pub unavailable_reason: Option<String>,
    pub pending_command: Option<EstablishObjectWorkspaceCommand>,
    pub workspace: Option<ObjectWorkspaceRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EstablishObjectWorkspaceCommand {
    pub schema_version: String,
    pub expected_state_hash: String,
    pub display_name: String,
    pub migration_object_name_or_scope: String,
    pub creator_attribution: String,
    pub idempotency_key: String,
}

#[derive(Clone, Debug)]
pub struct ObjectWorkspaceService {
    root: PathBuf,
    root_handle: Arc<File>,
}

#[derive(Debug)]
struct Inspection {
    status: ObjectWorkspaceStatus,
    descriptor: Option<LoadedDescriptor>,
    binding: Option<PersistedBinding>,
    pending: Option<PendingEstablishment>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LoadedDescriptor {
    descriptor: ObjectWorkspaceDescriptor,
    canonical_json: String,
    descriptor_hash: String,
}

#[derive(Clone, Debug)]
struct PersistedBinding {
    descriptor: ObjectWorkspaceDescriptor,
    descriptor_hash: String,
    descriptor_json: String,
    command_schema: String,
    idempotency_key: String,
    request_hash: String,
    canonical_request_json: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingEstablishment {
    action: ObjectWorkspaceAction,
    descriptor: LoadedDescriptor,
    command: EstablishObjectWorkspaceCommand,
    request_hash: String,
    canonical_request_json: String,
    lifecycle_evidence: PendingLifecycleEvidence,
    lifecycle_evidence_json: String,
    lifecycle_evidence_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PendingLifecycleEvidence {
    schema_version: String,
    action: ObjectWorkspaceAction,
    initial_database_present: bool,
    initial_user_version: i64,
    projected_tables: Vec<PendingEvidenceTable>,
    projected_database_hash: String,
    non_database_evidence_hash: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PendingEvidenceTable {
    table_name: String,
    columns: Vec<String>,
}

type PersistedBindingRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

type PendingEstablishmentRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

#[derive(Debug, Default)]
pub(crate) struct ObjectWorkspaceIntegrityReplay {
    pub(crate) checked_workspaces: usize,
    pub(crate) warnings: Vec<String>,
    pub(crate) issues: Vec<String>,
}

impl ObjectWorkspaceService {
    /// Bind lifecycle operations to the exact real folder selected at startup.
    ///
    /// # Errors
    ///
    /// Returns an error when the selected path is not a real, non-link
    /// directory or when its identity cannot be retained safely.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref();
        let metadata = fs::symlink_metadata(root).map_err(|error| KernelError::io(root, error))?;
        if !metadata.file_type().is_dir() || metadata_is_reparse_point(&metadata) {
            return Err(KernelError::IntegrityFailed(
                "the startup-selected workspace must be one real non-link directory".to_owned(),
            ));
        }
        let root = root
            .canonicalize()
            .map_err(|error| KernelError::io(root, error))?;
        let root_handle =
            retain_root_directory(&root).map_err(|error| KernelError::io(&root, error))?;
        Ok(Self {
            root,
            root_handle: Arc::new(root_handle),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Inspect lifecycle authority without creating or migrating `SQLite`.
    ///
    /// # Errors
    ///
    /// Returns an error when the retained root identity changed or the
    /// descriptor/database authority is missing, unsafe, or inconsistent.
    pub fn inspect(&self) -> Result<ObjectWorkspaceStatus> {
        self.verify_root_identity()?;
        inspect_workspace(&self.root).map(|inspection| inspection.status)
    }

    /// Require a byte-for-byte and row-for-row active descriptor binding.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace authority is not active or its
    /// retained root, descriptor, or database binding fails verification.
    pub fn require_active(&self) -> Result<ObjectWorkspaceRecord> {
        self.verify_root_identity()?;
        let inspection = inspect_workspace(&self.root)?;
        if inspection.status.state != ObjectWorkspaceState::Active {
            return Err(KernelError::Conflict(
                inspection
                    .status
                    .unavailable_reason
                    .unwrap_or_else(|| "object workspace authority is not active".to_owned()),
            ));
        }
        inspection.status.workspace.ok_or_else(|| {
            KernelError::IntegrityFailed(
                "active object workspace has no verified identity".to_owned(),
            )
        })
    }

    /// Require exact active descriptor/database authority before opening the
    /// writer that may apply a supported `SQLite` schema migration.
    ///
    /// Unlike ordinary inspection, this narrow preflight accepts an older
    /// supported schema only when the current object-workspace descriptor and
    /// immutable binding already agree. The locked writer independently
    /// retains that authority before migrating.
    #[cfg_attr(test, allow(dead_code))]
    pub(crate) fn require_active_for_writer_migration(&self) -> Result<()> {
        self.verify_root_identity()?;
        let inspection = inspect_workspace_with_schema_policy(&self.root, false)?;
        if inspection.status.state != ObjectWorkspaceState::Active {
            return Err(KernelError::Conflict(
                inspection
                    .status
                    .unavailable_reason
                    .unwrap_or_else(|| "object workspace authority is not active".to_owned()),
            ));
        }
        Ok(())
    }

    /// Initialize the startup-selected empty workspace.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is invalid, stale, or conflicting, or
    /// when the workspace cannot be established atomically.
    pub fn initialize(
        &self,
        command: &EstablishObjectWorkspaceCommand,
    ) -> Result<ObjectWorkspaceRecord> {
        self.establish(ObjectWorkspaceAction::Initialize, command)
    }

    /// Adopt existing durable evidence in the startup-selected workspace.
    ///
    /// # Errors
    ///
    /// Returns an error when the command is invalid, stale, or conflicting,
    /// the legacy evidence is unsafe, or adoption cannot complete atomically.
    pub fn adopt(
        &self,
        command: &EstablishObjectWorkspaceCommand,
    ) -> Result<ObjectWorkspaceRecord> {
        self.establish(ObjectWorkspaceAction::Adopt, command)
    }

    #[allow(clippy::too_many_lines)]
    fn establish(
        &self,
        action: ObjectWorkspaceAction,
        command: &EstablishObjectWorkspaceCommand,
    ) -> Result<ObjectWorkspaceRecord> {
        validate_command(action, command)?;
        self.verify_root_identity()?;
        let lock = WorkspaceLock::acquire(&self.root)?;
        self.verify_root_identity()?;
        let inspection = inspect_workspace(&self.root)?;
        let canonical_request_json = canonical_json_string(&serde_json::to_value(command)?)?;
        let request_hash = sha256_hex(canonical_request_json.as_bytes());

        if inspection.status.state == ObjectWorkspaceState::Active {
            let binding = inspection.binding.ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "active object workspace has no durable command identity".to_owned(),
                )
            })?;
            if binding.command_schema == action.command_schema()
                && binding.idempotency_key == command.idempotency_key
                && binding.request_hash == request_hash
                && binding.canonical_request_json == canonical_request_json
            {
                return Ok(ObjectWorkspaceRecord::from_descriptor(
                    &binding.descriptor,
                    &binding.descriptor_hash,
                ));
            }
            return Err(KernelError::Conflict(
                "this folder is already bound to a different object workspace identity".to_owned(),
            ));
        }
        let (descriptor, pending) = if let Some(pending) = inspection.pending.as_ref() {
            verify_pending_retry(
                pending,
                action,
                command,
                &request_hash,
                &canonical_request_json,
            )?;
            verify_pending_lifecycle_evidence(&self.root, pending)?;
            (pending.descriptor.clone(), pending.clone())
        } else {
            if inspection.status.required_action != Some(action) {
                return Err(KernelError::Conflict(match action {
                    ObjectWorkspaceAction::Initialize => {
                        "existing durable evidence must be explicitly adopted, not initialized"
                            .to_owned()
                    }
                    ObjectWorkspaceAction::Adopt => {
                        "an empty folder must be initialized, not adopted".to_owned()
                    }
                }));
            }
            if inspection.status.state_hash != command.expected_state_hash {
                return Err(KernelError::Conflict(
                    "object workspace lifecycle state changed; reload before retrying".to_owned(),
                ));
            }
            if action == ObjectWorkspaceAction::Adopt {
                verify_legacy_workspace_before_adoption(&self.root)?;
            }
            let lifecycle_evidence = capture_pending_lifecycle_evidence(&self.root, action)?;
            let descriptor =
                new_descriptor(action, command, &request_hash, &canonical_request_json)?;
            let canonical_json = canonical_json_string(&serde_json::to_value(&descriptor)?)?;
            let descriptor = LoadedDescriptor {
                descriptor,
                descriptor_hash: sha256_hex(canonical_json.as_bytes()),
                canonical_json,
            };
            let writer = lock.open_writer_for_pending_establishment()?;
            persist_pending(
                &writer,
                &descriptor,
                action,
                command,
                &request_hash,
                &canonical_request_json,
                &lifecycle_evidence,
            )?;
            drop(writer);
            let persisted = inspect_workspace(&self.root)?.pending.ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "pending workspace command was not recoverable before migration".to_owned(),
                )
            })?;
            verify_pending_lifecycle_evidence(&self.root, &persisted)?;
            (descriptor, persisted)
        };
        let descriptor_path = self.root.join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
        let created_descriptor = inspection.descriptor.is_none();
        let mut created_descriptor_handle = None;
        if created_descriptor {
            lock.verify()?;
            // Publication can fail after the destination name exists (for
            // example, a directory-sync failure). The immutable pending
            // command remains so an exact retry can verify and resume either
            // namespace outcome.
            created_descriptor_handle = Some(publish_descriptor(
                &self.root,
                descriptor.canonical_json.as_bytes(),
            )?);
        } else {
            let loaded = read_descriptor(&descriptor_path)?;
            if loaded.descriptor_hash != descriptor.descriptor_hash
                || loaded.canonical_json != descriptor.canonical_json
            {
                return Err(KernelError::IntegrityFailed(
                    "pending workspace descriptor differs from its immutable establishment ledger"
                        .to_owned(),
                ));
            }
        }
        if let Err(error) = lock.verify() {
            cleanup_failed_descriptor(&lock, &descriptor, created_descriptor_handle.take())?;
            return Err(error);
        }

        let mut writer = match lock
            .open_writer_for_establishment_with_descriptor(&descriptor.descriptor_hash)
        {
            Ok(writer) => writer,
            Err(error) => {
                cleanup_failed_descriptor(&lock, &descriptor, created_descriptor_handle.take())?;
                return Err(error);
            }
        };
        let guarded_root = self.root.clone();
        let guarded_pending = pending.clone();
        writer.install_supplemental_commit_guard(move || {
            verify_pending_lifecycle_evidence(&guarded_root, &guarded_pending)
        })?;
        let persist_result = persist_binding(
            &writer,
            &descriptor,
            &pending,
            action,
            command,
            &request_hash,
            &canonical_request_json,
        );
        if let Err(error) = persist_result {
            drop(writer);
            cleanup_failed_descriptor(&lock, &descriptor, created_descriptor_handle.take())?;
            return Err(error);
        }
        drop(writer);
        self.verify_root_identity()?;
        let active = inspect_workspace(&self.root)?;
        if active.status.state != ObjectWorkspaceState::Active {
            return Err(KernelError::IntegrityFailed(
                "object workspace establishment did not produce active authority".to_owned(),
            ));
        }
        active.status.workspace.ok_or_else(|| {
            KernelError::IntegrityFailed(
                "object workspace establishment lost its verified identity".to_owned(),
            )
        })
    }

    fn verify_root_identity(&self) -> Result<()> {
        let metadata =
            fs::symlink_metadata(&self.root).map_err(|error| KernelError::io(&self.root, error))?;
        if !metadata.file_type().is_dir() || metadata_is_reparse_point(&metadata) {
            return Err(KernelError::IntegrityFailed(
                "the startup-selected workspace path no longer names a real directory".to_owned(),
            ));
        }
        let canonical = self
            .root
            .canonicalize()
            .map_err(|error| KernelError::io(&self.root, error))?;
        if canonical != self.root
            || !root_directory_matches(&self.root, self.root_handle.as_ref())
                .map_err(|error| KernelError::io(&self.root, error))?
        {
            return Err(KernelError::IntegrityFailed(
                "the startup-selected workspace path changed identity".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn retain_root_directory(path: &Path) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW,
        Mode::empty(),
    )?;
    Ok(File::from(descriptor))
}

#[cfg(windows)]
fn retain_root_directory(path: &Path) -> std::io::Result<File> {
    crate::durability::open_directory_nofollow(path)
}

#[cfg(not(any(unix, windows)))]
fn retain_root_directory(path: &Path) -> std::io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn root_directory_matches(path: &Path, file: &File) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let named = fs::symlink_metadata(path)?;
    let opened = file.metadata()?;
    Ok(named.file_type().is_dir()
        && !named.file_type().is_symlink()
        && named.dev() == opened.dev()
        && named.ino() == opened.ino())
}

#[cfg(windows)]
fn root_directory_matches(path: &Path, file: &File) -> std::io::Result<bool> {
    crate::durability::path_names_open_directory(path, file)
}

#[cfg(not(any(unix, windows)))]
fn root_directory_matches(path: &Path, file: &File) -> std::io::Result<bool> {
    let named = fs::symlink_metadata(path)?;
    let opened = file.metadata()?;
    Ok(named.file_type().is_dir() && opened.file_type().is_dir())
}

/// Initialize an exact empty synthetic/local-automation workspace through the
/// same lifecycle authority used by the browser. Existing evidence is never
/// adopted by this helper; adoption always remains an explicit operator action.
///
/// # Errors
///
/// Returns an error when the selected workspace is unsafe, already requires
/// adoption, has a conflicting active identity, or cannot be initialized
/// atomically.
pub fn establish_for_local_automation(
    root: impl AsRef<Path>,
    display_name: &str,
    migration_object_name_or_scope: &str,
    creator_attribution: &str,
) -> Result<ObjectWorkspaceRecord> {
    let service = ObjectWorkspaceService::new(root)?;
    let status = service.inspect()?;
    if let Some(record) = status.workspace {
        if record.display_name == display_name
            && record.migration_object_name_or_scope == migration_object_name_or_scope
            && record.creator_attribution == creator_attribution
        {
            return Ok(record);
        }
        return Err(KernelError::Conflict(
            "active automated workspace identity differs from the requested fixture".to_owned(),
        ));
    }
    let action = status.required_action.ok_or_else(|| {
        KernelError::IntegrityFailed(
            "object workspace lifecycle did not expose one exact automation action".to_owned(),
        )
    })?;
    if action != ObjectWorkspaceAction::Initialize {
        return Err(KernelError::Conflict(
            "existing evidence requires explicit object workspace adoption".to_owned(),
        ));
    }
    let semantic = canonical_hash(&json!({
        "action": action,
        "displayName": display_name,
        "migrationObjectNameOrScope": migration_object_name_or_scope,
        "creatorAttribution": creator_attribution,
    }))?;
    let command = EstablishObjectWorkspaceCommand {
        schema_version: action.command_schema().to_owned(),
        expected_state_hash: status.state_hash,
        display_name: display_name.to_owned(),
        migration_object_name_or_scope: migration_object_name_or_scope.to_owned(),
        creator_attribution: creator_attribution.to_owned(),
        idempotency_key: format!("local-automation-{semantic}"),
    };
    service.initialize(&command)
}

/// Explicitly adopt one exact existing synthetic/local-automation workspace.
///
/// This helper never initializes an empty folder. It derives the bounded
/// adoption CAS from the evidence inspected in this invocation and then uses
/// the same locked lifecycle command as the browser.
///
/// # Errors
///
/// Returns an error when the selected workspace is unsafe, requires
/// initialization instead, has a conflicting active identity, or cannot be
/// adopted atomically.
pub fn adopt_for_local_automation(
    root: impl AsRef<Path>,
    display_name: &str,
    migration_object_name_or_scope: &str,
    creator_attribution: &str,
) -> Result<ObjectWorkspaceRecord> {
    let service = ObjectWorkspaceService::new(root)?;
    let status = service.inspect()?;
    if let Some(record) = status.workspace {
        if record.established_by == ObjectWorkspaceAction::Adopt.established_by()
            && record.display_name == display_name
            && record.migration_object_name_or_scope == migration_object_name_or_scope
            && record.creator_attribution == creator_attribution
        {
            return Ok(record);
        }
        return Err(KernelError::Conflict(
            "active automated workspace identity differs from the requested adoption".to_owned(),
        ));
    }
    let action = status.required_action.ok_or_else(|| {
        KernelError::IntegrityFailed(
            "object workspace lifecycle did not expose one exact automation action".to_owned(),
        )
    })?;
    if action != ObjectWorkspaceAction::Adopt {
        return Err(KernelError::Conflict(
            "empty evidence requires object workspace initialization".to_owned(),
        ));
    }
    let semantic = canonical_hash(&json!({
        "action": action,
        "displayName": display_name,
        "migrationObjectNameOrScope": migration_object_name_or_scope,
        "creatorAttribution": creator_attribution,
    }))?;
    let command = EstablishObjectWorkspaceCommand {
        schema_version: action.command_schema().to_owned(),
        expected_state_hash: status.state_hash,
        display_name: display_name.to_owned(),
        migration_object_name_or_scope: migration_object_name_or_scope.to_owned(),
        creator_attribution: creator_attribution.to_owned(),
        idempotency_key: format!("local-automation-{semantic}"),
    };
    service.adopt(&command)
}

pub(crate) fn replay_integrity(root: &Path) -> ObjectWorkspaceIntegrityReplay {
    let descriptor_present =
        fs::symlink_metadata(root.join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME)).is_ok();
    match ObjectWorkspaceService::new(root).and_then(|service| service.inspect()) {
        Ok(status) if status.state == ObjectWorkspaceState::Active => {
            ObjectWorkspaceIntegrityReplay {
                checked_workspaces: 1,
                ..ObjectWorkspaceIntegrityReplay::default()
            }
        }
        Ok(status) if descriptor_present => ObjectWorkspaceIntegrityReplay {
            issues: vec![format!(
                "object workspace descriptor is present without active SQLite authority ({:?})",
                status.state
            )],
            ..ObjectWorkspaceIntegrityReplay::default()
        },
        Ok(status) if status.state == ObjectWorkspaceState::AdoptRequired => {
            ObjectWorkspaceIntegrityReplay {
                warnings: vec![
                    "legacy workspace has no object workspace descriptor and requires explicit adoption"
                        .to_owned(),
                ],
                ..ObjectWorkspaceIntegrityReplay::default()
            }
        }
        Ok(_) => ObjectWorkspaceIntegrityReplay::default(),
        Err(_) => ObjectWorkspaceIntegrityReplay {
            issues: vec![
                "object workspace descriptor and SQLite authority failed exact replay".to_owned(),
            ],
            ..ObjectWorkspaceIntegrityReplay::default()
        },
    }
}

pub(crate) fn retain_writer_descriptor_authority(
    root: &Path,
    connection: &Connection,
) -> Result<(File, String)> {
    let descriptor = load_verified_active_writer_authority(root, connection)?;
    let descriptor_path = root.join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
    let handle = open_regular_nofollow(&descriptor_path)?;
    verify_retained_writer_descriptor(root, &handle, &descriptor.descriptor_hash)?;
    Ok((handle, descriptor.descriptor_hash))
}

pub(crate) fn verify_writer_database_authority(
    root: &Path,
    connection: &Connection,
    expected_descriptor_hash: &str,
) -> Result<()> {
    let descriptor = load_verified_active_writer_authority(root, connection)?;
    if descriptor.descriptor_hash != expected_descriptor_hash {
        return Err(KernelError::IntegrityFailed(
            "workspace descriptor hash changed across schema migration".to_owned(),
        ));
    }
    Ok(())
}

fn load_verified_active_writer_authority(
    root: &Path,
    connection: &Connection,
) -> Result<LoadedDescriptor> {
    let descriptor_path = root.join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
    let descriptor_present = match fs::symlink_metadata(&descriptor_path) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(KernelError::io(&descriptor_path, error)),
    };
    let binding_present = table_exists(connection, "object_workspace_bindings")?;
    match (descriptor_present, binding_present) {
        (false, false) => Err(KernelError::Conflict(
            "initialize or explicitly adopt this object workspace before mutation".to_owned(),
        )),
        (true, true) => {
            let user_version: i64 =
                connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if !(1..=SCHEMA_VERSION).contains(&user_version) {
                return Err(KernelError::IntegrityFailed(format!(
                    "active object workspace requires a supported schema version between 1 and {SCHEMA_VERSION}, found {user_version}"
                )));
            }
            let descriptor = read_descriptor(&descriptor_path)?;
            let binding = load_binding(connection)?.ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "object workspace binding table has no active singleton".to_owned(),
                )
            })?;
            if load_pending(connection)?.is_some() {
                return Err(KernelError::IntegrityFailed(
                    "active and pending object workspace authorities coexist".to_owned(),
                ));
            }
            verify_object_workspace_authority_guards(connection, true, false)?;
            verify_binding(&descriptor, &binding)?;
            verify_workspace_engagement_binding(connection, &binding.descriptor.workspace_id)?;
            Ok(descriptor)
        }
        _ => Err(KernelError::IntegrityFailed(
            "workspace descriptor and SQLite binding are incomplete".to_owned(),
        )),
    }
}

pub(crate) fn verify_retained_writer_descriptor(
    root: &Path,
    handle: &File,
    expected_hash: &str,
) -> Result<()> {
    let path = root.join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
    if !named_file_matches(&path, handle)? || hash_open_descriptor(handle)? != expected_hash {
        return Err(KernelError::IntegrityFailed(
            "workspace descriptor identity changed while writer authority was retained".to_owned(),
        ));
    }
    let loaded = read_descriptor(&path)?;
    if loaded.descriptor_hash != expected_hash {
        return Err(KernelError::IntegrityFailed(
            "workspace descriptor bytes changed while writer authority was retained".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn retain_pending_descriptor_authority(
    root: &Path,
    expected_hash: &str,
) -> Result<File> {
    let path = root.join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
    let loaded = read_descriptor(&path)?;
    if loaded.descriptor_hash != expected_hash {
        return Err(KernelError::IntegrityFailed(
            "pending workspace descriptor hash changed before binding".to_owned(),
        ));
    }
    let handle = open_regular_nofollow(&path)?;
    verify_retained_writer_descriptor(root, &handle, expected_hash)?;
    Ok(handle)
}

#[allow(clippy::too_many_lines)]
fn inspect_workspace(root: &Path) -> Result<Inspection> {
    inspect_workspace_with_schema_policy(root, true)
}

#[allow(clippy::too_many_lines)]
fn inspect_workspace_with_schema_policy(
    root: &Path,
    require_current_schema: bool,
) -> Result<Inspection> {
    let descriptor_path = root.join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
    let descriptor = match fs::symlink_metadata(&descriptor_path) {
        Ok(_) => Some(read_descriptor(&descriptor_path)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(KernelError::io(&descriptor_path, error)),
    };
    let database_path = root.join(DATABASE_FILENAME);
    let database_exists = path_is_safe_regular_file(&database_path)?;
    let (binding, pending, engagement_id) = if database_exists {
        let connection = open_read_only(&database_path)?;
        let user_version: i64 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if user_version > SCHEMA_VERSION {
            return Err(KernelError::IntegrityFailed(format!(
                "workspace database schema version {user_version} is newer than supported {SCHEMA_VERSION}"
            )));
        }
        let binding = load_binding(&connection)?;
        let pending = load_pending(&connection)?;
        if binding.is_some() && user_version < 1 {
            return Err(KernelError::IntegrityFailed(format!(
                "active object workspace requires a supported schema version between 1 and {SCHEMA_VERSION}, found {user_version}"
            )));
        }
        if binding.is_some() && pending.is_some() {
            return Err(KernelError::IntegrityFailed(
                "active and pending object workspace authorities coexist".to_owned(),
            ));
        }
        if binding.is_some() && require_current_schema && user_version != SCHEMA_VERSION {
            return Err(KernelError::IntegrityFailed(format!(
                "active object workspace requires schema version {SCHEMA_VERSION}, found {user_version}"
            )));
        }
        verify_object_workspace_authority_guards(
            &connection,
            binding.is_some(),
            pending.is_some(),
        )?;
        let engagement_id = binding
            .as_ref()
            .map(|binding| {
                verify_workspace_engagement_binding(&connection, &binding.descriptor.workspace_id)
            })
            .transpose()?
            .flatten();
        (binding, pending, engagement_id)
    } else {
        (None, None, None)
    };

    match (&descriptor, &binding) {
        (Some(descriptor), Some(binding)) => {
            verify_binding(descriptor, binding)?;
            let workspace = ObjectWorkspaceRecord::from_descriptor(
                &descriptor.descriptor,
                &descriptor.descriptor_hash,
            );
            let state_hash = canonical_hash(&json!({
                "schemaVersion": OBJECT_WORKSPACE_STATUS_SCHEMA,
                "state": "ACTIVE",
                "workspaceId": workspace.workspace_id,
                "descriptorHash": workspace.descriptor_hash,
                "engagementId": engagement_id,
            }))?;
            Ok(Inspection {
                status: ObjectWorkspaceStatus {
                    schema_version: OBJECT_WORKSPACE_STATUS_SCHEMA.to_owned(),
                    state: ObjectWorkspaceState::Active,
                    state_hash,
                    required_action: None,
                    unavailable_reason: None,
                    pending_command: None,
                    workspace: Some(workspace),
                },
                descriptor: Some(descriptor.clone()),
                binding: Some(binding.clone()),
                pending: None,
            })
        }
        (None, Some(_)) => Err(KernelError::IntegrityFailed(
            "SQLite object workspace authority exists but workspace.json is missing".to_owned(),
        )),
        _ => {
            if let Some(pending) = pending.as_ref() {
                if let Some(descriptor) = descriptor.as_ref()
                    && (descriptor.descriptor_hash != pending.descriptor.descriptor_hash
                        || descriptor.canonical_json != pending.descriptor.canonical_json)
                {
                    return Err(KernelError::IntegrityFailed(
                        "pending workspace descriptor differs from its immutable SQLite ledger"
                            .to_owned(),
                    ));
                }
                let state = match pending.action {
                    ObjectWorkspaceAction::Initialize => ObjectWorkspaceState::InitializeRequired,
                    ObjectWorkspaceAction::Adopt => ObjectWorkspaceState::AdoptRequired,
                };
                let state_hash = canonical_hash(&json!({
                    "schemaVersion": OBJECT_WORKSPACE_STATUS_SCHEMA,
                    "state": match state {
                        ObjectWorkspaceState::InitializeRequired => "INITIALIZE_REQUIRED",
                        ObjectWorkspaceState::AdoptRequired => "ADOPT_REQUIRED",
                        ObjectWorkspaceState::Active => unreachable!(),
                    },
                    "pendingRequestHash": pending.request_hash,
                    "pendingIdempotencyKey": pending.command.idempotency_key,
                    "pendingWorkspaceId": pending.descriptor.descriptor.workspace_id,
                    "pendingDescriptorPresent": descriptor.is_some(),
                    "pendingDescriptorHash": descriptor
                        .as_ref()
                        .map(|descriptor| descriptor.descriptor_hash.as_str()),
                }))?;
                let reason = match pending.action {
                    ObjectWorkspaceAction::Initialize => {
                        "Resume the exact interrupted workspace initialization before continuing."
                    }
                    ObjectWorkspaceAction::Adopt => {
                        "Resume the exact interrupted workspace adoption before continuing."
                    }
                };
                return Ok(Inspection {
                    status: ObjectWorkspaceStatus {
                        schema_version: OBJECT_WORKSPACE_STATUS_SCHEMA.to_owned(),
                        state,
                        state_hash,
                        required_action: Some(pending.action),
                        unavailable_reason: Some(reason.to_owned()),
                        pending_command: Some(pending.command.clone()),
                        workspace: None,
                    },
                    descriptor,
                    binding: None,
                    pending: Some(pending.clone()),
                });
            }
            if descriptor.is_some() {
                return Err(KernelError::IntegrityFailed(
                    "workspace.json has no immutable pending or active SQLite authority".to_owned(),
                ));
            }
            let legacy = classify_unbound_workspace(root, database_exists)?;
            let lifecycle_evidence_hash = if legacy {
                adoption_evidence_hash(root)?
            } else {
                sha256_hex(b"migration-stream/object-workspace/empty/v1")
            };
            let state = if legacy {
                ObjectWorkspaceState::AdoptRequired
            } else {
                ObjectWorkspaceState::InitializeRequired
            };
            let action = if legacy {
                ObjectWorkspaceAction::Adopt
            } else {
                ObjectWorkspaceAction::Initialize
            };
            let state_hash = canonical_hash(&json!({
                "schemaVersion": OBJECT_WORKSPACE_STATUS_SCHEMA,
                "state": match state {
                    ObjectWorkspaceState::InitializeRequired => "INITIALIZE_REQUIRED",
                    ObjectWorkspaceState::AdoptRequired => "ADOPT_REQUIRED",
                    ObjectWorkspaceState::Active => unreachable!(),
                },
                "lifecycleEvidenceHash": lifecycle_evidence_hash,
            }))?;
            let reason = match state {
                ObjectWorkspaceState::InitializeRequired => {
                    "Initialize this workspace before accepting source evidence."
                }
                ObjectWorkspaceState::AdoptRequired => {
                    "Adopt this existing local evidence before continuing."
                }
                ObjectWorkspaceState::Active => unreachable!(),
            };
            Ok(Inspection {
                status: ObjectWorkspaceStatus {
                    schema_version: OBJECT_WORKSPACE_STATUS_SCHEMA.to_owned(),
                    state,
                    state_hash,
                    required_action: Some(action),
                    unavailable_reason: Some(reason.to_owned()),
                    pending_command: None,
                    workspace: None,
                },
                descriptor,
                binding: None,
                pending: None,
            })
        }
    }
}

fn classify_unbound_workspace(root: &Path, database_exists: bool) -> Result<bool> {
    let mut unexpected_entries = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| KernelError::io(root, error))? {
        let entry = entry.map_err(|error| KernelError::io(root, error))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(
            name.as_ref(),
            ".migration-stream.lock"
                | OBJECT_WORKSPACE_DESCRIPTOR_FILENAME
                | "state.sqlite3"
                | "state.sqlite3-wal"
                | "state.sqlite3-shm"
        ) || name.starts_with("workspace.json.part.")
        {
            continue;
        }
        unexpected_entries.push(name.into_owned());
    }
    if !database_exists {
        if unexpected_entries.is_empty() {
            return Ok(false);
        }
        return Err(KernelError::IntegrityFailed(
            "workspace contains evidence files without a regular SQLite authority".to_owned(),
        ));
    }
    let connection = open_read_only(&root.join(DATABASE_FILENAME))?;
    let user_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if user_version > SCHEMA_VERSION {
        return Err(KernelError::IntegrityFailed(format!(
            "workspace database schema version {user_version} is newer than supported {SCHEMA_VERSION}"
        )));
    }
    let application_tables = application_table_names(&connection)?;
    if user_version == 0 && !application_tables.is_empty() {
        return Err(KernelError::IntegrityFailed(
            "workspace database is not a recognized Migration Stream schema".to_owned(),
        ));
    }
    if user_version == 0 && application_tables.is_empty() && unexpected_entries.is_empty() {
        // A failed pre-ledger bootstrap may have created an otherwise empty
        // SQLite file. It has no operator meaning or durable application
        // evidence and must remain eligible for initialization.
        return Ok(false);
    }
    if user_version > 0 && !application_tables.iter().any(|table| table == "jobs") {
        return Err(KernelError::IntegrityFailed(
            "workspace database schema fingerprint is incomplete".to_owned(),
        ));
    }
    // Any recognized SQLite file is existing durable workspace material. Even a
    // schema-only database must be explicitly adopted; initialization is
    // reserved for a genuinely empty startup-selected folder.
    Ok(true)
}

fn application_table_names(connection: &Connection) -> Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT name FROM sqlite_schema
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
         ORDER BY name",
    )?;
    statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn adoption_evidence_hash(root: &Path) -> Result<String> {
    let mut paths = Vec::new();
    collect_adoption_evidence_paths(root, root, &mut paths)?;
    paths.sort_by(|left, right| left.0.cmp(&right.0));
    if paths.len() > MAXIMUM_ADOPTION_EVIDENCE_FILES {
        return Err(KernelError::IntegrityFailed(format!(
            "existing workspace exceeds the {MAXIMUM_ADOPTION_EVIDENCE_FILES}-file adoption evidence bound"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let mut link_counts = HashMap::<(u64, u64), (u64, usize)>::new();
        for (_, path, _) in &paths {
            let metadata =
                fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
            let entry = link_counts
                .entry((metadata.dev(), metadata.ino()))
                .or_insert((metadata.nlink(), 0));
            if entry.0 != metadata.nlink() {
                return Err(KernelError::IntegrityFailed(
                    "existing workspace evidence changed during adoption inspection".to_owned(),
                ));
            }
            entry.1 = entry.1.checked_add(1).ok_or_else(|| {
                KernelError::IntegrityFailed(
                    "existing workspace hard-link count overflowed".to_owned(),
                )
            })?;
        }
        if link_counts
            .values()
            .any(|(named_links, in_workspace_links)| {
                usize::try_from(*named_links).ok() != Some(*in_workspace_links)
            })
        {
            return Err(KernelError::IntegrityFailed(
                "existing workspace adoption evidence must not have hard links outside the selected workspace"
                    .to_owned(),
            ));
        }
    }
    let mut total_bytes = 0_u64;
    let mut evidence = Sha256::new();
    evidence.update(b"migration-stream/object-workspace/adoption-evidence/v1\0");
    for (relative, path, length) in paths {
        total_bytes = total_bytes.checked_add(length).ok_or_else(|| {
            KernelError::IntegrityFailed(
                "existing workspace adoption evidence byte count overflowed".to_owned(),
            )
        })?;
        if total_bytes > MAXIMUM_ADOPTION_EVIDENCE_BYTES {
            return Err(KernelError::IntegrityFailed(format!(
                "existing workspace exceeds the {MAXIMUM_ADOPTION_EVIDENCE_BYTES}-byte adoption evidence bound"
            )));
        }
        let mut file = open_regular_nofollow(&path)?;
        let mut file_hash = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
        let mut read_bytes = 0_u64;
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|error| KernelError::io(&path, error))?;
            if count == 0 {
                break;
            }
            read_bytes = read_bytes
                .checked_add(u64::try_from(count).unwrap_or(u64::MAX))
                .ok_or_else(|| {
                    KernelError::IntegrityFailed(
                        "existing workspace evidence length overflowed".to_owned(),
                    )
                })?;
            if read_bytes > length {
                return Err(KernelError::IntegrityFailed(
                    "existing workspace evidence changed during adoption inspection".to_owned(),
                ));
            }
            file_hash.update(&buffer[..count]);
        }
        if read_bytes != length || !named_file_matches(&path, &file)? {
            return Err(KernelError::IntegrityFailed(
                "existing workspace evidence changed during adoption inspection".to_owned(),
            ));
        }
        let relative_bytes = relative.as_bytes();
        evidence.update(
            u64::try_from(relative_bytes.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        evidence.update(relative_bytes);
        evidence.update(length.to_be_bytes());
        evidence.update(file_hash.finalize());
    }
    evidence.update(total_bytes.to_be_bytes());
    Ok(crate::canonical::lower_hex(evidence.finalize()))
}

fn collect_adoption_evidence_paths(
    root: &Path,
    directory: &Path,
    paths: &mut Vec<(String, PathBuf, u64)>,
) -> Result<()> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| KernelError::io(directory, error))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| KernelError::io(directory, error))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(|_| {
            KernelError::IntegrityFailed(
                "existing workspace evidence escaped its selected root".to_owned(),
            )
        })?;
        let relative = relative.to_str().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "existing workspace evidence paths must be valid UTF-8".to_owned(),
            )
        })?;
        if directory == root
            && matches!(
                relative,
                OBJECT_WORKSPACE_DESCRIPTOR_FILENAME
                    | ".migration-stream.lock"
                    | "state.sqlite3-shm"
            )
        {
            continue;
        }
        if directory == root && relative.starts_with("workspace.json.part.") {
            continue;
        }
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| KernelError::io(&path, error))?;
        if metadata_is_reparse_point(&metadata) || metadata.file_type().is_symlink() {
            return Err(KernelError::IntegrityFailed(
                "existing workspace adoption evidence must not contain links or reparse points"
                    .to_owned(),
            ));
        }
        if metadata.file_type().is_dir() {
            collect_adoption_evidence_paths(root, &path, paths)?;
        } else if metadata.file_type().is_file() {
            verify_single_file_link(&path, &metadata)?;
            paths.push((relative.replace('\\', "/"), path, metadata.len()));
        } else {
            return Err(KernelError::IntegrityFailed(
                "existing workspace adoption evidence contains a nonregular entry".to_owned(),
            ));
        }
    }
    Ok(())
}

fn capture_pending_lifecycle_evidence(
    root: &Path,
    action: ObjectWorkspaceAction,
) -> Result<PendingLifecycleEvidence> {
    let database_path = root.join(DATABASE_FILENAME);
    let initial_database_present = path_is_safe_regular_file(&database_path)?;
    let (initial_user_version, projected_tables, projected_database_hash) =
        if initial_database_present {
            let connection = open_read_only(&database_path)?;
            let user_version: i64 =
                connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if user_version > SCHEMA_VERSION {
                return Err(KernelError::IntegrityFailed(format!(
                    "workspace database schema version {user_version} is newer than supported {SCHEMA_VERSION}"
                )));
            }
            let projected_tables = capture_projected_tables(&connection)?;
            let projected_database_hash = projected_database_hash(&connection, &projected_tables)?;
            (user_version, projected_tables, projected_database_hash)
        } else {
            (
                0,
                Vec::new(),
                sha256_hex(b"migration-stream/object-workspace/projected-database/v1\0"),
            )
        };
    let evidence = PendingLifecycleEvidence {
        schema_version: PENDING_LIFECYCLE_EVIDENCE_SCHEMA.to_owned(),
        action,
        initial_database_present,
        initial_user_version,
        projected_tables,
        projected_database_hash,
        non_database_evidence_hash: pending_non_database_evidence_hash(root)?,
    };
    verify_pending_lifecycle_evidence_contract(&evidence, action)?;
    Ok(evidence)
}

fn verify_pending_lifecycle_evidence(root: &Path, pending: &PendingEstablishment) -> Result<()> {
    verify_pending_lifecycle_evidence_contract(&pending.lifecycle_evidence, pending.action)?;
    if pending.lifecycle_evidence_hash != sha256_hex(pending.lifecycle_evidence_json.as_bytes())
        || canonical_json_string(&serde_json::to_value(&pending.lifecycle_evidence)?)?
            != pending.lifecycle_evidence_json
    {
        return Err(KernelError::IntegrityFailed(
            "pending workspace lifecycle evidence is not canonically hash-bound".to_owned(),
        ));
    }
    let current_non_database_hash = pending_non_database_evidence_hash(root)?;
    if current_non_database_hash != pending.lifecycle_evidence.non_database_evidence_hash {
        return Err(KernelError::Conflict(
            "workspace evidence changed after the establishment command was recorded".to_owned(),
        ));
    }

    let database_path = root.join(DATABASE_FILENAME);
    if !path_is_safe_regular_file(&database_path)? {
        return Err(KernelError::IntegrityFailed(
            "pending workspace command lost its regular SQLite authority".to_owned(),
        ));
    }
    let connection = open_read_only(&database_path)?;
    let user_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if user_version < pending.lifecycle_evidence.initial_user_version
        || user_version > SCHEMA_VERSION
    {
        return Err(KernelError::IntegrityFailed(
            "pending workspace database schema left its bounded migration range".to_owned(),
        ));
    }
    let current_projected_hash =
        projected_database_hash(&connection, &pending.lifecycle_evidence.projected_tables)?;
    if current_projected_hash != pending.lifecycle_evidence.projected_database_hash {
        return Err(KernelError::Conflict(
            "workspace SQLite evidence changed after the establishment command was recorded"
                .to_owned(),
        ));
    }
    verify_no_unbound_rows_outside_projection(
        &connection,
        &pending.lifecycle_evidence.projected_tables,
    )?;
    Ok(())
}

fn verify_pending_lifecycle_evidence_contract(
    evidence: &PendingLifecycleEvidence,
    action: ObjectWorkspaceAction,
) -> Result<()> {
    if evidence.schema_version != PENDING_LIFECYCLE_EVIDENCE_SCHEMA
        || evidence.action != action
        || evidence.initial_user_version < 0
        || evidence.initial_user_version > SCHEMA_VERSION
        || !is_sha256(&evidence.projected_database_hash)
        || !is_sha256(&evidence.non_database_evidence_hash)
    {
        return Err(KernelError::IntegrityFailed(
            "pending workspace lifecycle evidence has an invalid bounded contract".to_owned(),
        ));
    }
    if action == ObjectWorkspaceAction::Initialize
        && (evidence.initial_user_version != 0 || !evidence.projected_tables.is_empty())
    {
        return Err(KernelError::IntegrityFailed(
            "workspace initialization evidence was not empty".to_owned(),
        ));
    }
    if action == ObjectWorkspaceAction::Adopt
        && (!evidence.initial_database_present || evidence.projected_tables.is_empty())
    {
        return Err(KernelError::IntegrityFailed(
            "workspace adoption evidence did not contain recognized SQLite authority".to_owned(),
        ));
    }
    let mut previous_table: Option<&str> = None;
    for table in &evidence.projected_tables {
        if table.table_name.is_empty()
            || table.columns.is_empty()
            || previous_table.is_some_and(|previous| previous >= table.table_name.as_str())
        {
            return Err(KernelError::IntegrityFailed(
                "pending workspace SQLite projection is not canonical".to_owned(),
            ));
        }
        let mut previous_column: Option<&str> = None;
        for column in &table.columns {
            if column.is_empty()
                || previous_column.is_some_and(|previous| previous == column.as_str())
            {
                return Err(KernelError::IntegrityFailed(
                    "pending workspace SQLite column projection is not canonical".to_owned(),
                ));
            }
            previous_column = Some(column);
        }
        previous_table = Some(&table.table_name);
    }
    Ok(())
}

fn capture_projected_tables(connection: &Connection) -> Result<Vec<PendingEvidenceTable>> {
    let mut tables = application_table_names(connection)?
        .into_iter()
        .filter(|table| !table.starts_with("object_workspace_"))
        .map(|table_name| {
            let mut statement = connection.prepare(
                "SELECT name FROM pragma_table_info(?1)
                 ORDER BY cid",
            )?;
            let columns = statement
                .query_map([&table_name], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if columns.is_empty() {
                return Err(KernelError::IntegrityFailed(format!(
                    "workspace evidence table {table_name:?} has no inspectable columns"
                )));
            }
            Ok(PendingEvidenceTable {
                table_name,
                columns,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    tables.sort_by(|left, right| left.table_name.cmp(&right.table_name));
    Ok(tables)
}

fn projected_database_hash(
    connection: &Connection,
    tables: &[PendingEvidenceTable],
) -> Result<String> {
    let mut database = Sha256::new();
    database.update(b"migration-stream/object-workspace/projected-database/v1\0");
    for table in tables {
        hash_length_delimited(&mut database, table.table_name.as_bytes());
        for column in &table.columns {
            hash_length_delimited(&mut database, column.as_bytes());
        }
        let select = format!(
            "SELECT {} FROM {}",
            table
                .columns
                .iter()
                .map(|column| quote_sqlite_identifier(column))
                .collect::<Vec<_>>()
                .join(", "),
            quote_sqlite_identifier(&table.table_name)
        );
        let mut statement = connection.prepare(&select)?;
        let mut rows = statement.query([])?;
        let mut row_hashes = Vec::new();
        while let Some(row) = rows.next()? {
            let mut row_hash = Sha256::new();
            row_hash.update(b"migration-stream/object-workspace/projected-row/v1\0");
            for index in 0..table.columns.len() {
                match row.get_ref(index)? {
                    ValueRef::Null => row_hash.update([0]),
                    ValueRef::Integer(value) => {
                        row_hash.update([1]);
                        row_hash.update(value.to_be_bytes());
                    }
                    ValueRef::Real(value) => {
                        row_hash.update([2]);
                        row_hash.update(value.to_bits().to_be_bytes());
                    }
                    ValueRef::Text(value) => {
                        row_hash.update([3]);
                        hash_length_delimited(&mut row_hash, value);
                    }
                    ValueRef::Blob(value) => {
                        row_hash.update([4]);
                        hash_length_delimited(&mut row_hash, value);
                    }
                }
            }
            row_hashes.push(row_hash.finalize().to_vec());
        }
        row_hashes.sort();
        database.update(
            u64::try_from(row_hashes.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        for row_hash in row_hashes {
            database.update(row_hash);
        }
    }
    Ok(crate::canonical::lower_hex(database.finalize()))
}

fn verify_no_unbound_rows_outside_projection(
    connection: &Connection,
    projection: &[PendingEvidenceTable],
) -> Result<()> {
    let projected_names = projection
        .iter()
        .map(|table| table.table_name.as_str())
        .collect::<std::collections::HashSet<_>>();
    for table_name in application_table_names(connection)? {
        if table_name == "object_workspace_pending_establishments"
            || projected_names.contains(table_name.as_str())
        {
            continue;
        }
        let query = format!(
            "SELECT EXISTS(SELECT 1 FROM {} LIMIT 1)",
            quote_sqlite_identifier(&table_name)
        );
        let has_rows: bool = connection.query_row(&query, [], |row| row.get(0))?;
        if has_rows {
            return Err(KernelError::Conflict(
                "workspace SQLite evidence gained rows after the establishment command was recorded"
                    .to_owned(),
            ));
        }
    }
    for table in projection {
        let current_columns = {
            let mut statement = connection.prepare(
                "SELECT name FROM pragma_table_info(?1)
                 ORDER BY cid",
            )?;
            statement
                .query_map([&table.table_name], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        if !table
            .columns
            .iter()
            .all(|column| current_columns.contains(column))
        {
            return Err(KernelError::IntegrityFailed(
                "workspace SQLite projection lost a recorded column".to_owned(),
            ));
        }
        let additional_columns = current_columns
            .iter()
            .filter(|column| !table.columns.contains(column))
            .collect::<Vec<_>>();
        if !additional_columns.is_empty() {
            let predicate = additional_columns
                .iter()
                .map(|column| format!("{} IS NOT NULL", quote_sqlite_identifier(column)))
                .collect::<Vec<_>>()
                .join(" OR ");
            let query = format!(
                "SELECT EXISTS(SELECT 1 FROM {} WHERE {predicate} LIMIT 1)",
                quote_sqlite_identifier(&table.table_name)
            );
            let has_non_null_addition: bool = connection.query_row(&query, [], |row| row.get(0))?;
            if has_non_null_addition {
                return Err(KernelError::Conflict(
                    "workspace SQLite evidence changed in a migration-added column".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn quote_sqlite_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn hash_length_delimited(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

fn pending_non_database_evidence_hash(root: &Path) -> Result<String> {
    let mut paths = Vec::new();
    collect_pending_non_database_paths(root, root, &mut paths)?;
    paths.sort_by(|left, right| left.0.cmp(&right.0));
    let mut evidence = Sha256::new();
    evidence.update(b"migration-stream/object-workspace/non-database-evidence/v1\0");
    let mut total_bytes = 0_u64;
    for (relative, path, length) in paths {
        total_bytes = total_bytes.checked_add(length).ok_or_else(|| {
            KernelError::IntegrityFailed(
                "workspace lifecycle evidence byte count overflowed".to_owned(),
            )
        })?;
        if total_bytes > MAXIMUM_ADOPTION_EVIDENCE_BYTES {
            return Err(KernelError::IntegrityFailed(format!(
                "existing workspace exceeds the {MAXIMUM_ADOPTION_EVIDENCE_BYTES}-byte lifecycle evidence bound"
            )));
        }
        let mut file = open_regular_nofollow(&path)?;
        let mut file_hash = Sha256::new();
        let mut copied = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
        loop {
            let read = std::io::Read::read(&mut file, &mut buffer)
                .map_err(|error| KernelError::io(&path, error))?;
            if read == 0 {
                break;
            }
            file_hash.update(&buffer[..read]);
            copied = copied
                .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
                .ok_or_else(|| {
                    KernelError::IntegrityFailed("evidence read length overflowed".to_owned())
                })?;
        }
        if copied != length || !named_file_matches(&path, &file)? {
            return Err(KernelError::IntegrityFailed(
                "workspace lifecycle evidence changed during inspection".to_owned(),
            ));
        }
        hash_length_delimited(&mut evidence, relative.as_bytes());
        evidence.update(length.to_be_bytes());
        evidence.update(file_hash.finalize());
    }
    evidence.update(total_bytes.to_be_bytes());
    Ok(crate::canonical::lower_hex(evidence.finalize()))
}

fn collect_pending_non_database_paths(
    root: &Path,
    directory: &Path,
    paths: &mut Vec<(String, PathBuf, u64)>,
) -> Result<()> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| KernelError::io(directory, error))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| KernelError::io(directory, error))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(|_| {
            KernelError::IntegrityFailed(
                "workspace lifecycle evidence escaped its selected root".to_owned(),
            )
        })?;
        let relative = relative.to_str().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "workspace lifecycle evidence paths must be valid UTF-8".to_owned(),
            )
        })?;
        if directory == root
            && matches!(
                relative,
                OBJECT_WORKSPACE_DESCRIPTOR_FILENAME
                    | ".migration-stream.lock"
                    | "state.sqlite3"
                    | "state.sqlite3-wal"
                    | "state.sqlite3-shm"
            )
        {
            continue;
        }
        if directory == root && relative.starts_with("workspace.json.part.") {
            continue;
        }
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| KernelError::io(&path, error))?;
        if metadata_is_reparse_point(&metadata) || metadata.file_type().is_symlink() {
            return Err(KernelError::IntegrityFailed(
                "workspace lifecycle evidence must not contain links or reparse points".to_owned(),
            ));
        }
        if metadata.file_type().is_dir() {
            collect_pending_non_database_paths(root, &path, paths)?;
        } else if metadata.file_type().is_file() {
            verify_single_file_link(&path, &metadata)?;
            paths.push((relative.replace('\\', "/"), path, metadata.len()));
        } else {
            return Err(KernelError::IntegrityFailed(
                "workspace lifecycle evidence contains a nonregular entry".to_owned(),
            ));
        }
    }
    if paths.len() > MAXIMUM_ADOPTION_EVIDENCE_FILES {
        return Err(KernelError::IntegrityFailed(format!(
            "existing workspace exceeds the {MAXIMUM_ADOPTION_EVIDENCE_FILES}-file lifecycle evidence bound"
        )));
    }
    Ok(())
}

fn verify_single_file_link(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        let _ = path;
        if metadata.nlink() != 1 {
            return Err(KernelError::IntegrityFailed(
                "workspace evidence files must not have hard-link aliases".to_owned(),
            ));
        }
    }
    #[cfg(windows)]
    {
        let _ = metadata;
        let file = open_regular_nofollow(path)?;
        if crate::durability::file_link_count(&file)
            .map_err(|error| KernelError::io(path, error))?
            != 1
        {
            return Err(KernelError::IntegrityFailed(
                "workspace evidence files must not have hard-link aliases".to_owned(),
            ));
        }
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (path, metadata);
    Ok(())
}

fn verify_legacy_workspace_before_adoption(root: &Path) -> Result<()> {
    let database = root.join(DATABASE_FILENAME);
    let connection = open_read_only(&database)?;
    let user_version: i64 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if user_version > SCHEMA_VERSION {
        return Err(KernelError::IntegrityFailed(format!(
            "workspace database schema version {user_version} is newer than supported {SCHEMA_VERSION}"
        )));
    }
    let sqlite_result: String =
        connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if sqlite_result != "ok" {
        return Err(KernelError::IntegrityFailed(
            "existing workspace SQLite evidence failed bounded pre-adoption integrity".to_owned(),
        ));
    }
    let foreign_key_issue: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check LIMIT 1)",
            [],
            |row| row.get(0),
        )
        .unwrap_or(false);
    if foreign_key_issue {
        return Err(KernelError::IntegrityFailed(
            "existing workspace foreign-key evidence failed bounded pre-adoption integrity"
                .to_owned(),
        ));
    }
    drop(connection);
    if user_version == SCHEMA_VERSION {
        let report = integrity_check(root)?;
        if !report.issues.is_empty() {
            return Err(KernelError::IntegrityFailed(
                "existing workspace evidence failed global integrity replay before adoption"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

fn new_descriptor(
    action: ObjectWorkspaceAction,
    command: &EstablishObjectWorkspaceCommand,
    request_hash: &str,
    canonical_request_json: &str,
) -> Result<ObjectWorkspaceDescriptor> {
    let descriptor = ObjectWorkspaceDescriptor {
        schema_version: OBJECT_WORKSPACE_DESCRIPTOR_SCHEMA.to_owned(),
        workspace_id: Uuid::new_v4().to_string(),
        display_name: command.display_name.clone(),
        migration_object_name_or_scope: command.migration_object_name_or_scope.clone(),
        creator_attribution: command.creator_attribution.clone(),
        established_by: action.established_by().to_owned(),
        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        command_schema: command.schema_version.clone(),
        idempotency_key: command.idempotency_key.clone(),
        request_hash: request_hash.to_owned(),
        canonical_request_json: canonical_request_json.to_owned(),
    };
    verify_descriptor_contract(&descriptor)?;
    Ok(descriptor)
}

fn verify_descriptor_contract(descriptor: &ObjectWorkspaceDescriptor) -> Result<()> {
    if descriptor.schema_version != OBJECT_WORKSPACE_DESCRIPTOR_SCHEMA {
        return Err(KernelError::IntegrityFailed(
            "workspace descriptor schema version is unsupported".to_owned(),
        ));
    }
    let workspace_id = Uuid::parse_str(&descriptor.workspace_id).map_err(|_| {
        KernelError::IntegrityFailed("workspace descriptor ID is not a canonical UUID".to_owned())
    })?;
    if workspace_id.to_string() != descriptor.workspace_id {
        return Err(KernelError::IntegrityFailed(
            "workspace descriptor ID is not canonical".to_owned(),
        ));
    }
    validate_text(&descriptor.display_name, "workspace display name")?;
    validate_text(
        &descriptor.migration_object_name_or_scope,
        "migration object name or scope",
    )?;
    validate_text(&descriptor.creator_attribution, "creator attribution")?;
    if !matches!(
        descriptor.established_by.as_str(),
        "INITIALIZED" | "ADOPTED"
    ) {
        return Err(KernelError::IntegrityFailed(
            "workspace descriptor establishment mode is invalid".to_owned(),
        ));
    }
    let parsed = DateTime::parse_from_rfc3339(&descriptor.created_at).map_err(|_| {
        KernelError::IntegrityFailed("workspace descriptor timestamp is invalid".to_owned())
    })?;
    if parsed.to_rfc3339_opts(SecondsFormat::Millis, true) != descriptor.created_at {
        return Err(KernelError::IntegrityFailed(
            "workspace descriptor timestamp is not canonical UTC milliseconds".to_owned(),
        ));
    }
    let action = match descriptor.established_by.as_str() {
        "INITIALIZED" => ObjectWorkspaceAction::Initialize,
        "ADOPTED" => ObjectWorkspaceAction::Adopt,
        _ => unreachable!(),
    };
    if descriptor.command_schema != action.command_schema()
        || !is_sha256(&descriptor.request_hash)
        || descriptor.request_hash != sha256_hex(descriptor.canonical_request_json.as_bytes())
    {
        return Err(KernelError::IntegrityFailed(
            "workspace descriptor command ledger is invalid".to_owned(),
        ));
    }
    let request_value =
        parse_unique_json(descriptor.canonical_request_json.as_bytes()).map_err(|error| {
            KernelError::IntegrityFailed(format!(
                "workspace descriptor command ledger is invalid: {error}"
            ))
        })?;
    if canonical_json_string(&request_value)? != descriptor.canonical_request_json {
        return Err(KernelError::IntegrityFailed(
            "workspace descriptor command ledger is not canonical JSON".to_owned(),
        ));
    }
    let command: EstablishObjectWorkspaceCommand =
        serde_json::from_value(request_value).map_err(|error| {
            KernelError::IntegrityFailed(format!(
                "workspace descriptor command ledger has an invalid contract: {error}"
            ))
        })?;
    validate_command(action, &command).map_err(|_| {
        KernelError::IntegrityFailed(
            "workspace descriptor command ledger failed bounded replay".to_owned(),
        )
    })?;
    if command.schema_version != descriptor.command_schema
        || command.idempotency_key != descriptor.idempotency_key
        || command.display_name != descriptor.display_name
        || command.migration_object_name_or_scope != descriptor.migration_object_name_or_scope
        || command.creator_attribution != descriptor.creator_attribution
    {
        return Err(KernelError::IntegrityFailed(
            "workspace descriptor identity differs from its immutable command ledger".to_owned(),
        ));
    }
    Ok(())
}

fn validate_command(
    action: ObjectWorkspaceAction,
    command: &EstablishObjectWorkspaceCommand,
) -> Result<()> {
    if command.schema_version != action.command_schema() {
        return Err(KernelError::PolicyInvalid(format!(
            "object workspace command schema must be {:?}",
            action.command_schema()
        )));
    }
    if !is_sha256(&command.expected_state_hash) {
        return Err(KernelError::PolicyInvalid(
            "object workspace expected state hash is invalid".to_owned(),
        ));
    }
    validate_text(&command.display_name, "workspace display name")?;
    validate_text(
        &command.migration_object_name_or_scope,
        "migration object name or scope",
    )?;
    validate_text(&command.creator_attribution, "creator attribution")?;
    let key = command.idempotency_key.as_bytes();
    if key.len() < MINIMUM_IDEMPOTENCY_KEY_BYTES
        || key.len() > MAXIMUM_IDEMPOTENCY_KEY_BYTES
        || !command
            .idempotency_key
            .bytes()
            .enumerate()
            .all(|(index, byte)| {
                byte.is_ascii_alphanumeric()
                    || (index > 0 && matches!(byte, b'.' | b'_' | b':' | b'-'))
            })
    {
        return Err(KernelError::PolicyInvalid(
            "object workspace idempotency key is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_text(value: &str, label: &str) -> Result<()> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > MAXIMUM_TEXT_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must be 1..={MAXIMUM_TEXT_BYTES} UTF-8 bytes without surrounding whitespace or controls"
        )));
    }
    Ok(())
}

fn publish_descriptor(root: &Path, bytes: &[u8]) -> Result<File> {
    let partial = root.join(format!(
        "{OBJECT_WORKSPACE_DESCRIPTOR_FILENAME}.part.{}",
        Uuid::new_v4()
    ));
    let descriptor = root.join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
    let write_result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&partial)
            .map_err(|error| KernelError::io(&partial, error))?;
        file.write_all(bytes)
            .map_err(|error| KernelError::io(&partial, error))?;
        file.sync_all()
            .map_err(|error| KernelError::io(&partial, error))?;
        let mut permissions = file
            .metadata()
            .map_err(|error| KernelError::io(&partial, error))?
            .permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&partial, permissions)
            .map_err(|error| KernelError::io(&partial, error))?;
        file.sync_all()
            .map_err(|error| KernelError::io(&partial, error))?;
        drop(file);
        publish_new(&partial, &descriptor).map_err(|error| KernelError::io(&descriptor, error))?;
        #[cfg(test)]
        {
            let mut injected = DESCRIPTOR_PUBLISH_FAILURE_AFTER_NAMESPACE
                .lock()
                .expect("descriptor publication test hook");
            if injected.as_deref() == Some(root) {
                injected.take();
                return Err(KernelError::io(
                    &descriptor,
                    std::io::Error::other(
                        "injected failure after descriptor namespace publication",
                    ),
                ));
            }
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&partial);
    }
    write_result?;
    let loaded = read_descriptor(&descriptor)?;
    if loaded.canonical_json.as_bytes() != bytes {
        return Err(KernelError::IntegrityFailed(
            "published workspace descriptor differs from the synchronized attempt bytes".to_owned(),
        ));
    }
    let handle = open_regular_nofollow(&descriptor)?;
    if !named_file_matches(&descriptor, &handle)? {
        return Err(KernelError::IntegrityFailed(
            "published workspace descriptor changed identity".to_owned(),
        ));
    }
    Ok(handle)
}

fn cleanup_failed_descriptor(
    lock: &WorkspaceLock,
    descriptor: &LoadedDescriptor,
    created_descriptor: Option<File>,
) -> Result<()> {
    if let Some(handle) = created_descriptor {
        cleanup_created_descriptor(lock.root(), descriptor, handle)?;
    }
    Ok(())
}

fn cleanup_created_descriptor(
    root: &Path,
    expected: &LoadedDescriptor,
    created: File,
) -> Result<()> {
    let descriptor = root.join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME);
    if !named_file_matches(&descriptor, &created)? {
        return Err(KernelError::IntegrityFailed(
            "failed establishment did not remove a replaced workspace descriptor".to_owned(),
        ));
    }
    let loaded = read_descriptor(&descriptor)?;
    if loaded.descriptor_hash != expected.descriptor_hash
        || loaded.canonical_json != expected.canonical_json
    {
        return Err(KernelError::IntegrityFailed(
            "failed establishment did not remove a changed workspace descriptor".to_owned(),
        ));
    }
    let opened_hash = hash_open_descriptor(&created)?;
    if opened_hash != expected.descriptor_hash {
        return Err(KernelError::IntegrityFailed(
            "failed establishment retained descriptor bytes differ from the created attempt"
                .to_owned(),
        ));
    }
    let mut permissions = created
        .metadata()
        .map_err(|error| KernelError::io(&descriptor, error))?
        .permissions();
    make_owner_writable(&mut permissions);
    #[cfg(windows)]
    let created_identity = {
        let attribute_writer = open_regular_nofollow_for_attribute_write(&descriptor)?;
        let created_identity =
            migration_stream_windows_native::file_identity_from_open_file(&created)
                .map_err(|error| KernelError::io(&descriptor, error))?;
        let writer_identity =
            migration_stream_windows_native::file_identity_from_open_file(&attribute_writer)
                .map_err(|error| KernelError::io(&descriptor, error))?;
        if writer_identity != created_identity {
            return Err(KernelError::IntegrityFailed(
                "failed establishment descriptor changed identity before permission cleanup"
                    .to_owned(),
            ));
        }
        attribute_writer
            .set_permissions(permissions)
            .map_err(|error| KernelError::io(&descriptor, error))?;
        created_identity
    };
    #[cfg(not(windows))]
    created
        .set_permissions(permissions)
        .map_err(|error| KernelError::io(&descriptor, error))?;
    if !named_file_matches(&descriptor, &created)? {
        return Err(KernelError::IntegrityFailed(
            "failed establishment descriptor changed identity before cleanup".to_owned(),
        ));
    }
    #[cfg(windows)]
    {
        drop(created);
        migration_stream_windows_native::delete_file_exact(&descriptor, created_identity)
            .map_err(|error| KernelError::io(&descriptor, error))?;
    }
    #[cfg(not(windows))]
    {
        fs::remove_file(&descriptor).map_err(|error| KernelError::io(&descriptor, error))?;
        drop(created);
    }
    sync_workspace_directory_after_removal(root)
}

fn hash_open_descriptor(file: &File) -> Result<String> {
    let mut file = file
        .try_clone()
        .map_err(|error| KernelError::io(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME, error))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| KernelError::io(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME, error))?;
    let mut bytes = Vec::new();
    file.take(MAXIMUM_DESCRIPTOR_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| KernelError::io(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME, error))?;
    if bytes.len() > usize::try_from(MAXIMUM_DESCRIPTOR_BYTES).unwrap_or(usize::MAX) {
        return Err(KernelError::IntegrityFailed(
            "retained workspace descriptor exceeded its read bound".to_owned(),
        ));
    }
    Ok(sha256_hex(&bytes))
}

// The platform-neutral call site needs an I/O-shaped result even though Windows
// has no directory fsync equivalent to replay here.
#[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
fn sync_workspace_directory_after_removal(root: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| KernelError::io(root, error))
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        Ok(())
    }
}

fn verify_pending_retry(
    pending: &PendingEstablishment,
    action: ObjectWorkspaceAction,
    command: &EstablishObjectWorkspaceCommand,
    request_hash: &str,
    canonical_request_json: &str,
) -> Result<()> {
    if pending.action != action
        || pending.command != *command
        || pending.request_hash != request_hash
        || pending.canonical_request_json != canonical_request_json
        || pending.descriptor.descriptor.command_schema != action.command_schema()
        || pending.descriptor.descriptor.idempotency_key != command.idempotency_key
        || pending.descriptor.descriptor.request_hash != request_hash
        || pending.descriptor.descriptor.canonical_request_json != canonical_request_json
    {
        return Err(KernelError::Conflict(
            "only the exact original object workspace establishment command may resume".to_owned(),
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn persist_pending(
    writer: &WorkspaceWriter,
    descriptor: &LoadedDescriptor,
    action: ObjectWorkspaceAction,
    command: &EstablishObjectWorkspaceCommand,
    request_hash: &str,
    canonical_request_json: &str,
    lifecycle_evidence: &PendingLifecycleEvidence,
) -> Result<()> {
    let lifecycle_evidence_json =
        canonical_json_string(&serde_json::to_value(lifecycle_evidence)?)?;
    let lifecycle_evidence_hash = sha256_hex(lifecycle_evidence_json.as_bytes());
    let transaction = writer.connection().unchecked_transaction()?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS object_workspace_pending_establishments (
             singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
             action TEXT NOT NULL CHECK (action IN ('INITIALIZE', 'ADOPT')),
             descriptor_hash TEXT NOT NULL UNIQUE CHECK (
                 length(descriptor_hash) = 64
             ),
             descriptor_json TEXT NOT NULL CHECK (
                 length(CAST(descriptor_json AS BLOB)) BETWEEN 2 AND 16384
             ),
             workspace_id TEXT NOT NULL UNIQUE,
             command_schema TEXT NOT NULL,
             idempotency_key TEXT NOT NULL UNIQUE,
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_request_json AS BLOB)) BETWEEN 2 AND 4096
             ),
             lifecycle_evidence_json TEXT NOT NULL CHECK (
                 length(CAST(lifecycle_evidence_json AS BLOB))
                     BETWEEN 2 AND 1048576
             ),
             lifecycle_evidence_hash TEXT NOT NULL UNIQUE CHECK (
                 length(lifecycle_evidence_hash) = 64
             )
         );
         CREATE TRIGGER IF NOT EXISTS object_workspace_pending_establishments_no_update
             BEFORE UPDATE ON object_workspace_pending_establishments
             BEGIN
                 SELECT RAISE(ABORT, 'pending object workspace authority is immutable');
             END;
         CREATE TRIGGER IF NOT EXISTS object_workspace_pending_establishments_consume_only
             BEFORE DELETE ON object_workspace_pending_establishments
             WHEN NOT EXISTS (
                 SELECT 1
                 FROM object_workspace_bindings AS binding
                 WHERE binding.singleton = OLD.singleton
                   AND binding.workspace_id = OLD.workspace_id
                   AND binding.descriptor_hash = OLD.descriptor_hash
                   AND binding.descriptor_json = OLD.descriptor_json
                   AND binding.command_schema = OLD.command_schema
                   AND binding.idempotency_key = OLD.idempotency_key
                   AND binding.request_hash = OLD.request_hash
                   AND binding.canonical_request_json = OLD.canonical_request_json
                   AND (
                       (OLD.action = 'INITIALIZE'
                           AND binding.established_by = 'INITIALIZED')
                       OR (OLD.action = 'ADOPT'
                           AND binding.established_by = 'ADOPTED')
                   )
             )
             BEGIN
                 SELECT RAISE(ABORT, 'pending object workspace authority may only be consumed by its exact binding');
             END;",
    )?;
    let changed = transaction.execute(
        "INSERT INTO object_workspace_pending_establishments (
             singleton, action, descriptor_hash, descriptor_json, workspace_id,
             command_schema, idempotency_key, request_hash, canonical_request_json,
             lifecycle_evidence_json, lifecycle_evidence_hash
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            match action {
                ObjectWorkspaceAction::Initialize => "INITIALIZE",
                ObjectWorkspaceAction::Adopt => "ADOPT",
            },
            descriptor.descriptor_hash,
            descriptor.canonical_json,
            descriptor.descriptor.workspace_id,
            command.schema_version,
            command.idempotency_key,
            request_hash,
            canonical_request_json,
            lifecycle_evidence_json,
            lifecycle_evidence_hash,
        ],
    )?;
    if changed != 1 {
        return Err(KernelError::IntegrityFailed(
            "pending object workspace authority was not durably recorded".to_owned(),
        ));
    }
    verify_object_workspace_authority_guards(&transaction, false, true)?;
    let pending = load_pending(&transaction)?.ok_or_else(|| {
        KernelError::IntegrityFailed(
            "object workspace binding has no immutable pending establishment authority".to_owned(),
        )
    })?;
    verify_pending_retry(
        &pending,
        action,
        command,
        request_hash,
        canonical_request_json,
    )?;
    if pending.descriptor.descriptor_hash != descriptor.descriptor_hash
        || pending.descriptor.canonical_json != descriptor.canonical_json
        || pending.lifecycle_evidence != *lifecycle_evidence
        || pending.lifecycle_evidence_json != lifecycle_evidence_json
        || pending.lifecycle_evidence_hash != lifecycle_evidence_hash
    {
        return Err(KernelError::IntegrityFailed(
            "object workspace descriptor or evidence differs from its pending establishment authority"
                .to_owned(),
        ));
    }
    transaction.commit().map_err(Into::into)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn persist_binding(
    writer: &WorkspaceWriter,
    descriptor: &LoadedDescriptor,
    pending: &PendingEstablishment,
    action: ObjectWorkspaceAction,
    command: &EstablishObjectWorkspaceCommand,
    request_hash: &str,
    canonical_request_json: &str,
) -> Result<()> {
    let transaction = writer.connection().unchecked_transaction()?;
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS object_workspace_bindings (
             singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
             descriptor_schema TEXT NOT NULL,
             workspace_id TEXT NOT NULL UNIQUE,
             display_name TEXT NOT NULL,
             migration_object_name_or_scope TEXT NOT NULL,
             creator_attribution TEXT NOT NULL,
             established_by TEXT NOT NULL CHECK (
                 established_by IN ('INITIALIZED', 'ADOPTED')
             ),
             created_at TEXT NOT NULL,
             descriptor_hash TEXT NOT NULL UNIQUE CHECK (
                 length(descriptor_hash) = 64
             ),
             descriptor_json TEXT NOT NULL CHECK (
                 length(CAST(descriptor_json AS BLOB)) BETWEEN 2 AND 16384
             ),
             command_schema TEXT NOT NULL,
             idempotency_key TEXT NOT NULL UNIQUE,
             request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
             canonical_request_json TEXT NOT NULL CHECK (
                 length(CAST(canonical_request_json AS BLOB)) BETWEEN 2 AND 4096
             )
         );
         CREATE TRIGGER IF NOT EXISTS object_workspace_bindings_no_update
             BEFORE UPDATE ON object_workspace_bindings
             BEGIN
                 SELECT RAISE(ABORT, 'object workspace binding is immutable');
             END;
         CREATE TRIGGER IF NOT EXISTS object_workspace_bindings_no_delete
             BEFORE DELETE ON object_workspace_bindings
             BEGIN
                 SELECT RAISE(ABORT, 'object workspace binding is immutable');
             END;

         CREATE TABLE IF NOT EXISTS object_workspace_engagement_bindings (
             singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
             workspace_id TEXT NOT NULL UNIQUE
                 REFERENCES object_workspace_bindings(workspace_id)
                 ON DELETE RESTRICT,
             engagement_id TEXT NOT NULL UNIQUE
                 REFERENCES engagement_records(engagement_id)
                 ON DELETE RESTRICT,
             bound_at TEXT NOT NULL CHECK (
                 length(CAST(bound_at AS BLOB)) BETWEEN 1 AND 64
             )
         );
         CREATE TRIGGER IF NOT EXISTS object_workspace_engagement_bindings_no_update
             BEFORE UPDATE ON object_workspace_engagement_bindings
             BEGIN
                 SELECT RAISE(ABORT, 'object workspace engagement binding is immutable');
             END;
         CREATE TRIGGER IF NOT EXISTS object_workspace_engagement_bindings_no_delete
             BEFORE DELETE ON object_workspace_engagement_bindings
             BEGIN
                 SELECT RAISE(ABORT, 'object workspace engagement binding is immutable');
             END;",
    )?;
    verify_object_workspace_authority_guards(&transaction, true, true)?;
    let current_pending = load_pending(&transaction)?.ok_or_else(|| {
        KernelError::IntegrityFailed(
            "object workspace binding lost its pending establishment authority".to_owned(),
        )
    })?;
    if current_pending != *pending {
        return Err(KernelError::IntegrityFailed(
            "object workspace pending authority changed before final binding".to_owned(),
        ));
    }
    transaction.execute(
        "INSERT INTO object_workspace_bindings (
             singleton, descriptor_schema, workspace_id, display_name,
             migration_object_name_or_scope, creator_attribution,
             established_by, created_at, descriptor_hash, descriptor_json,
             command_schema, idempotency_key, request_hash,
             canonical_request_json
         ) VALUES (
             1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
         )",
        params![
            descriptor.descriptor.schema_version,
            descriptor.descriptor.workspace_id,
            descriptor.descriptor.display_name,
            descriptor.descriptor.migration_object_name_or_scope,
            descriptor.descriptor.creator_attribution,
            action.established_by(),
            descriptor.descriptor.created_at,
            descriptor.descriptor_hash,
            descriptor.canonical_json,
            command.schema_version,
            command.idempotency_key,
            request_hash,
            canonical_request_json,
        ],
    )?;
    transaction.execute(
        "INSERT INTO object_workspace_engagement_bindings (
             singleton, workspace_id, engagement_id, bound_at
         )
         SELECT 1, ?1, engagement_id, ?2
         FROM engagement_records
         WHERE singleton = 1",
        params![
            descriptor.descriptor.workspace_id,
            descriptor.descriptor.created_at
        ],
    )?;
    let deleted = transaction.execute(
        "DELETE FROM object_workspace_pending_establishments
         WHERE singleton = 1 AND descriptor_hash = ?1 AND request_hash = ?2",
        params![descriptor.descriptor_hash, request_hash],
    )?;
    if deleted != 1 {
        return Err(KernelError::IntegrityFailed(
            "pending object workspace authority was not consumed atomically".to_owned(),
        ));
    }
    #[cfg(test)]
    inject_final_binding_evidence_mutation(&writer.root)?;
    transaction.commit().map_err(Into::into)
}

#[cfg(test)]
fn inject_final_binding_evidence_mutation(root: &Path) -> Result<()> {
    let mutation = {
        let mut pending = FINAL_BINDING_EVIDENCE_MUTATION
            .lock()
            .expect("final binding mutation test hook");
        if pending
            .as_ref()
            .is_some_and(|(expected_root, _, _)| expected_root == root)
        {
            pending.take()
        } else {
            None
        }
    };
    if let Some((_, relative_path, bytes)) = mutation {
        let path = root.join(relative_path);
        fs::write(&path, bytes).map_err(|error| KernelError::io(&path, error))?;
    }
    Ok(())
}

pub(crate) fn bind_workspace_engagement(
    transaction: &Transaction<'_>,
    workspace_id: &str,
    engagement_id: &str,
    bound_at: &str,
) -> Result<()> {
    let changed = transaction.execute(
        "INSERT INTO object_workspace_engagement_bindings (
             singleton, workspace_id, engagement_id, bound_at
         ) VALUES (1, ?1, ?2, ?3)",
        params![workspace_id, engagement_id, bound_at],
    )?;
    if changed != 1 {
        return Err(KernelError::IntegrityFailed(
            "object workspace engagement identity was not durably bound".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn verify_workspace_engagement_binding(
    connection: &Connection,
    workspace_id: &str,
) -> Result<Option<String>> {
    if !table_exists(connection, "object_workspace_engagement_bindings")? {
        return Err(KernelError::IntegrityFailed(
            "active object workspace has no engagement-binding authority".to_owned(),
        ));
    }
    let engagement_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM engagement_records", [], |row| {
            row.get(0)
        })?;
    let binding_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM object_workspace_engagement_bindings",
        [],
        |row| row.get(0),
    )?;
    match (engagement_count, binding_count) {
        (0, 0) => Ok(None),
        (1, 1) => {
            let (bound_workspace_id, bound_engagement_id): (String, String) = connection
                .query_row(
                    "SELECT workspace_id, engagement_id
                     FROM object_workspace_engagement_bindings
                     WHERE singleton = 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
            let active_engagement_id: String = connection.query_row(
                "SELECT engagement_id FROM engagement_records WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            if bound_workspace_id != workspace_id || bound_engagement_id != active_engagement_id {
                return Err(KernelError::IntegrityFailed(
                    "object workspace and active engagement identities do not agree".to_owned(),
                ));
            }
            Ok(Some(active_engagement_id))
        }
        _ => Err(KernelError::IntegrityFailed(format!(
            "object workspace has {engagement_count} engagement identities and {binding_count} engagement bindings; expected matching singleton authority"
        ))),
    }
}

fn load_binding(connection: &Connection) -> Result<Option<PersistedBinding>> {
    if !table_exists(connection, "object_workspace_bindings")? {
        return Ok(None);
    }
    let row: Option<PersistedBindingRow> = connection
        .query_row(
            "SELECT descriptor_schema, workspace_id, display_name,
                    migration_object_name_or_scope, creator_attribution,
                    established_by, created_at, descriptor_hash,
                    descriptor_json, command_schema, idempotency_key,
                    request_hash, canonical_request_json
             FROM object_workspace_bindings WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                ))
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Err(KernelError::IntegrityFailed(
            "object workspace binding table has no singleton row".to_owned(),
        ));
    };
    let descriptor = ObjectWorkspaceDescriptor {
        schema_version: row.0,
        workspace_id: row.1,
        display_name: row.2,
        migration_object_name_or_scope: row.3,
        creator_attribution: row.4,
        established_by: row.5,
        created_at: row.6,
        command_schema: row.9.clone(),
        idempotency_key: row.10.clone(),
        request_hash: row.11.clone(),
        canonical_request_json: row.12.clone(),
    };
    Ok(Some(PersistedBinding {
        descriptor,
        descriptor_hash: row.7,
        descriptor_json: row.8,
        command_schema: row.9,
        idempotency_key: row.10,
        request_hash: row.11,
        canonical_request_json: row.12,
    }))
}

#[allow(clippy::too_many_lines)]
fn load_pending(connection: &Connection) -> Result<Option<PendingEstablishment>> {
    if !table_exists(connection, "object_workspace_pending_establishments")? {
        return Ok(None);
    }
    let row: Option<PendingEstablishmentRow> = connection
        .query_row(
            "SELECT action, descriptor_hash, descriptor_json, workspace_id,
                    command_schema, idempotency_key, request_hash,
                    canonical_request_json, lifecycle_evidence_json,
                    lifecycle_evidence_hash
             FROM object_workspace_pending_establishments
             WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Ok(None);
    };
    let action = match row.0.as_str() {
        "INITIALIZE" => ObjectWorkspaceAction::Initialize,
        "ADOPT" => ObjectWorkspaceAction::Adopt,
        _ => {
            return Err(KernelError::IntegrityFailed(
                "pending object workspace authority has an invalid action".to_owned(),
            ));
        }
    };
    let descriptor_value = parse_unique_json(row.2.as_bytes()).map_err(|error| {
        KernelError::IntegrityFailed(format!(
            "pending object workspace descriptor is invalid: {error}"
        ))
    })?;
    if canonical_json_string(&descriptor_value)? != row.2 {
        return Err(KernelError::IntegrityFailed(
            "pending object workspace descriptor is not canonical JSON".to_owned(),
        ));
    }
    let descriptor_document: ObjectWorkspaceDescriptor = serde_json::from_value(descriptor_value)
        .map_err(|error| {
        KernelError::IntegrityFailed(format!(
            "pending object workspace descriptor has an invalid contract: {error}"
        ))
    })?;
    verify_descriptor_contract(&descriptor_document)?;
    let descriptor = LoadedDescriptor {
        descriptor: descriptor_document,
        descriptor_hash: row.1,
        canonical_json: row.2,
    };
    if descriptor.descriptor_hash != sha256_hex(descriptor.canonical_json.as_bytes())
        || descriptor.descriptor.workspace_id != row.3
        || descriptor.descriptor.command_schema != row.4
        || descriptor.descriptor.idempotency_key != row.5
        || descriptor.descriptor.request_hash != row.6
        || descriptor.descriptor.canonical_request_json != row.7
        || descriptor.descriptor.established_by != action.established_by()
    {
        return Err(KernelError::IntegrityFailed(
            "pending object workspace authority fields do not agree".to_owned(),
        ));
    }
    let request_value = parse_unique_json(row.7.as_bytes()).map_err(|error| {
        KernelError::IntegrityFailed(format!(
            "pending object workspace command is invalid: {error}"
        ))
    })?;
    if canonical_json_string(&request_value)? != row.7 || row.6 != sha256_hex(row.7.as_bytes()) {
        return Err(KernelError::IntegrityFailed(
            "pending object workspace command is not canonical or hash-bound".to_owned(),
        ));
    }
    let command: EstablishObjectWorkspaceCommand =
        serde_json::from_value(request_value).map_err(|error| {
            KernelError::IntegrityFailed(format!(
                "pending object workspace command has an invalid contract: {error}"
            ))
        })?;
    validate_command(action, &command).map_err(|_| {
        KernelError::IntegrityFailed(
            "pending object workspace command failed bounded replay".to_owned(),
        )
    })?;
    if command.schema_version != row.4 || command.idempotency_key != row.5 {
        return Err(KernelError::IntegrityFailed(
            "pending object workspace command differs from its immutable ledger".to_owned(),
        ));
    }
    let lifecycle_value = parse_unique_json(row.8.as_bytes()).map_err(|error| {
        KernelError::IntegrityFailed(format!(
            "pending object workspace evidence is invalid: {error}"
        ))
    })?;
    if canonical_json_string(&lifecycle_value)? != row.8 || row.9 != sha256_hex(row.8.as_bytes()) {
        return Err(KernelError::IntegrityFailed(
            "pending object workspace evidence is not canonical or hash-bound".to_owned(),
        ));
    }
    let lifecycle_evidence: PendingLifecycleEvidence = serde_json::from_value(lifecycle_value)
        .map_err(|error| {
            KernelError::IntegrityFailed(format!(
                "pending object workspace evidence has an invalid contract: {error}"
            ))
        })?;
    verify_pending_lifecycle_evidence_contract(&lifecycle_evidence, action)?;
    Ok(Some(PendingEstablishment {
        action,
        descriptor,
        command,
        request_hash: row.6,
        canonical_request_json: row.7,
        lifecycle_evidence,
        lifecycle_evidence_json: row.8,
        lifecycle_evidence_hash: row.9,
    }))
}

fn verify_binding(descriptor: &LoadedDescriptor, binding: &PersistedBinding) -> Result<()> {
    verify_descriptor_contract(&descriptor.descriptor)?;
    if binding.descriptor != descriptor.descriptor
        || binding.descriptor_json != descriptor.canonical_json
        || binding.descriptor_hash != descriptor.descriptor_hash
        || binding.descriptor_hash != sha256_hex(binding.descriptor_json.as_bytes())
        || binding.request_hash != sha256_hex(binding.canonical_request_json.as_bytes())
        || binding.command_schema
            != match descriptor.descriptor.established_by.as_str() {
                "INITIALIZED" => INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA,
                "ADOPTED" => ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA,
                _ => unreachable!(),
            }
    {
        return Err(KernelError::IntegrityFailed(
            "workspace.json and SQLite object workspace authority do not agree".to_owned(),
        ));
    }
    let request =
        parse_unique_json(binding.canonical_request_json.as_bytes()).map_err(|error| {
            KernelError::IntegrityFailed(format!(
                "object workspace command evidence is invalid: {error}"
            ))
        })?;
    if canonical_json_string(&request)? != binding.canonical_request_json {
        return Err(KernelError::IntegrityFailed(
            "object workspace command evidence is not canonical JSON".to_owned(),
        ));
    }
    let command: EstablishObjectWorkspaceCommand =
        serde_json::from_value(request).map_err(|error| {
            KernelError::IntegrityFailed(format!(
                "object workspace command evidence has an invalid contract: {error}"
            ))
        })?;
    let action = match binding.command_schema.as_str() {
        INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA => ObjectWorkspaceAction::Initialize,
        ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA => ObjectWorkspaceAction::Adopt,
        _ => {
            return Err(KernelError::IntegrityFailed(
                "object workspace binding has an unsupported command schema".to_owned(),
            ));
        }
    };
    validate_command(action, &command).map_err(|_| {
        KernelError::IntegrityFailed(
            "object workspace command evidence failed bounded replay".to_owned(),
        )
    })?;
    if command.idempotency_key != binding.idempotency_key
        || command.display_name != descriptor.descriptor.display_name
        || command.migration_object_name_or_scope
            != descriptor.descriptor.migration_object_name_or_scope
        || command.creator_attribution != descriptor.descriptor.creator_attribution
    {
        return Err(KernelError::IntegrityFailed(
            "object workspace command evidence differs from its immutable descriptor".to_owned(),
        ));
    }
    Ok(())
}

fn verify_object_workspace_authority_guards(
    connection: &Connection,
    active: bool,
    pending: bool,
) -> Result<()> {
    let mut expected = Vec::new();
    if active {
        expected.extend([
            (
                "object_workspace_bindings",
                "object_workspace_bindings_no_update",
                BINDING_NO_UPDATE_TRIGGER,
            ),
            (
                "object_workspace_bindings",
                "object_workspace_bindings_no_delete",
                BINDING_NO_DELETE_TRIGGER,
            ),
            (
                "object_workspace_engagement_bindings",
                "object_workspace_engagement_bindings_no_update",
                ENGAGEMENT_BINDING_NO_UPDATE_TRIGGER,
            ),
            (
                "object_workspace_engagement_bindings",
                "object_workspace_engagement_bindings_no_delete",
                ENGAGEMENT_BINDING_NO_DELETE_TRIGGER,
            ),
        ]);
    }
    if pending {
        expected.extend([
            (
                "object_workspace_pending_establishments",
                "object_workspace_pending_establishments_no_update",
                PENDING_NO_UPDATE_TRIGGER,
            ),
            (
                "object_workspace_pending_establishments",
                "object_workspace_pending_establishments_consume_only",
                PENDING_CONSUME_ONLY_TRIGGER,
            ),
        ]);
    }
    for (table, name, expected_sql) in expected {
        let actual: Option<String> = connection
            .query_row(
                "SELECT sql FROM sqlite_schema
                 WHERE type = 'trigger' AND name = ?1 AND tbl_name = ?2",
                params![name, table],
                |row| row.get(0),
            )
            .optional()?;
        let expected_sql = normalized_authority_sql(expected_sql);
        if actual.as_deref().map(normalized_authority_sql).as_deref() != Some(expected_sql.as_str())
        {
            return Err(KernelError::IntegrityFailed(format!(
                "object workspace immutable guard {name} is missing or altered"
            )));
        }
    }
    Ok(())
}

fn normalized_authority_sql(value: &str) -> String {
    value
        .trim()
        .trim_end_matches(';')
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn read_descriptor(path: &Path) -> Result<LoadedDescriptor> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.file_type().is_file() || metadata_is_reparse_point(&metadata) {
        return Err(KernelError::IntegrityFailed(
            "workspace.json must be one real regular non-link file".to_owned(),
        ));
    }
    if !metadata.permissions().readonly() {
        return Err(KernelError::IntegrityFailed(
            "workspace.json must be read-only".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return Err(KernelError::IntegrityFailed(
                "workspace.json must not have hard-link aliases".to_owned(),
            ));
        }
    }
    if metadata.len() == 0 || metadata.len() > MAXIMUM_DESCRIPTOR_BYTES {
        return Err(KernelError::IntegrityFailed(format!(
            "workspace.json must be 1..={MAXIMUM_DESCRIPTOR_BYTES} bytes"
        )));
    }
    let file = open_regular_nofollow(path)?;
    #[cfg(windows)]
    if crate::durability::file_link_count(&file).map_err(|error| KernelError::io(path, error))? != 1
    {
        return Err(KernelError::IntegrityFailed(
            "workspace.json must not have hard-link aliases".to_owned(),
        ));
    }
    let opened = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    if opened.len() != metadata.len() {
        return Err(KernelError::IntegrityFailed(
            "workspace.json changed during bounded inspection".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    let mut reader = file.take(MAXIMUM_DESCRIPTOR_BYTES + 1);
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| KernelError::io(path, error))?;
    let file = reader.into_inner();
    if bytes.len() > usize::try_from(MAXIMUM_DESCRIPTOR_BYTES).unwrap_or(usize::MAX) {
        return Err(KernelError::IntegrityFailed(
            "workspace.json exceeded its read bound".to_owned(),
        ));
    }
    if !named_file_matches(path, &file)? {
        return Err(KernelError::IntegrityFailed(
            "workspace.json changed identity during bounded inspection".to_owned(),
        ));
    }
    let value = parse_unique_json(&bytes).map_err(|error| {
        KernelError::IntegrityFailed(format!("workspace.json is invalid: {error}"))
    })?;
    let canonical_json = canonical_json_string(&value)?;
    if canonical_json.as_bytes() != bytes {
        return Err(KernelError::IntegrityFailed(
            "workspace.json is not exact canonical JSON".to_owned(),
        ));
    }
    let descriptor: ObjectWorkspaceDescriptor = serde_json::from_value(value).map_err(|error| {
        KernelError::IntegrityFailed(format!(
            "workspace.json does not satisfy its strict descriptor contract: {error}"
        ))
    })?;
    verify_descriptor_contract(&descriptor)?;
    Ok(LoadedDescriptor {
        descriptor,
        descriptor_hash: sha256_hex(canonical_json.as_bytes()),
        canonical_json,
    })
}

#[cfg(unix)]
fn named_file_matches(path: &Path, file: &File) -> Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    let opened = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    Ok(named.file_type().is_file()
        && !named.file_type().is_symlink()
        && named.dev() == opened.dev()
        && named.ino() == opened.ino()
        && named.len() == opened.len())
}

#[cfg(windows)]
fn named_file_matches(path: &Path, file: &File) -> Result<bool> {
    crate::durability::path_names_open_file(path, file)
        .map_err(|error| KernelError::io(path, error))
}

#[cfg(not(any(unix, windows)))]
fn named_file_matches(path: &Path, file: &File) -> Result<bool> {
    let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    let opened = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    Ok(named.file_type().is_file() && named.len() == opened.len())
}

#[cfg(unix)]
fn open_regular_nofollow(path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| {
        let error: std::io::Error = error.into();
        KernelError::io(path, error)
    })?;
    let file = File::from(descriptor);
    if !file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?
        .file_type()
        .is_file()
    {
        return Err(KernelError::IntegrityFailed(
            "workspace.json is not a regular file".to_owned(),
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn open_regular_nofollow_for_attribute_write(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let file = OpenOptions::new()
        .access_mode(GENERIC_READ | FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| KernelError::io(path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    if !metadata.file_type().is_file() || crate::durability::is_reparse_point(&metadata) {
        return Err(KernelError::IntegrityFailed(
            "workspace.json permission cleanup target is not a regular non-link file".to_owned(),
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_regular_nofollow(path: &Path) -> Result<File> {
    #[cfg(windows)]
    {
        crate::durability::open_file_nofollow(path).map_err(|error| KernelError::io(path, error))
    }
    #[cfg(not(windows))]
    {
        File::open(path).map_err(|error| KernelError::io(path, error))
    }
}

fn path_is_safe_regular_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata_is_reparse_point(&metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                if metadata.nlink() != 1 {
                    return Err(KernelError::IntegrityFailed(
                        "workspace database must not have hard-link aliases".to_owned(),
                    ));
                }
            }
            #[cfg(windows)]
            if crate::durability::file_link_count(&open_regular_nofollow(path)?)
                .map_err(|error| KernelError::io(path, error))?
                != 1
            {
                return Err(KernelError::IntegrityFailed(
                    "workspace database must not have hard-link aliases".to_owned(),
                ));
            }
            Ok(true)
        }
        Ok(_) => Err(KernelError::IntegrityFailed(
            "workspace database must be one real regular non-link file".to_owned(),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(KernelError::io(path, error)),
    }
}

fn table_exists(connection: &Connection, table: &str) -> Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM sqlite_schema
                 WHERE type = 'table' AND name = ?1
             )",
            [table],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        crate::durability::is_reparse_point(metadata)
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    crate::canonical::lower_hex(Sha256::digest(bytes))
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    fn command(
        status: &ObjectWorkspaceStatus,
        schema_version: &str,
    ) -> EstablishObjectWorkspaceCommand {
        EstablishObjectWorkspaceCommand {
            schema_version: schema_version.to_owned(),
            expected_state_hash: status.state_hash.clone(),
            display_name: "Local readiness workspace".to_owned(),
            migration_object_name_or_scope: "Customer migration object".to_owned(),
            creator_attribution: "Local operator".to_owned(),
            idempotency_key: "workspace-command-0001".to_owned(),
        }
    }

    fn persist_pending_only(
        service: &ObjectWorkspaceService,
        action: ObjectWorkspaceAction,
        command: &EstablishObjectWorkspaceCommand,
    ) -> LoadedDescriptor {
        let canonical_request =
            canonical_json_string(&serde_json::to_value(command).expect("canonical command value"))
                .expect("canonical command");
        let request_hash = sha256_hex(canonical_request.as_bytes());
        let descriptor =
            new_descriptor(action, command, &request_hash, &canonical_request).expect("descriptor");
        let canonical_descriptor =
            canonical_json_string(&serde_json::to_value(&descriptor).expect("descriptor JSON"))
                .expect("canonical descriptor");
        let descriptor = LoadedDescriptor {
            descriptor,
            descriptor_hash: sha256_hex(canonical_descriptor.as_bytes()),
            canonical_json: canonical_descriptor,
        };
        let lifecycle_evidence = capture_pending_lifecycle_evidence(service.root(), action)
            .expect("pending lifecycle evidence");
        let lock = WorkspaceLock::acquire(service.root()).expect("workspace lock");
        let writer = lock
            .open_writer_for_pending_establishment()
            .expect("pending-only writer");
        persist_pending(
            &writer,
            &descriptor,
            action,
            command,
            &request_hash,
            &canonical_request,
            &lifecycle_evidence,
        )
        .expect("persist pending authority");
        descriptor
    }

    #[test]
    fn fresh_workspace_initializes_and_exact_retry_reopens_identity() {
        let temporary = tempfile::tempdir().expect("temporary");
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let initial = service.inspect().expect("initial");
        assert_eq!(initial.state, ObjectWorkspaceState::InitializeRequired);
        let command = command(&initial, INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA);
        let first = service.initialize(&command).expect("initialize");
        let second = service.initialize(&command).expect("exact retry");
        assert_eq!(first, second);
        assert_eq!(
            service.inspect().expect("active").state,
            ObjectWorkspaceState::Active
        );
        assert!(temporary.path().join("workspace.json").is_file());
    }

    #[test]
    fn initialize_rejects_changed_or_conflicting_retry() {
        let temporary = tempfile::tempdir().expect("temporary");
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let initial = service.inspect().expect("initial");
        let command = command(&initial, INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA);
        service.initialize(&command).expect("initialize");
        let mut conflict = command;
        conflict.display_name = "Different workspace".to_owned();
        assert!(matches!(
            service.initialize(&conflict),
            Err(KernelError::Conflict(_))
        ));
    }

    #[test]
    fn descriptor_tamper_fails_closed() {
        let temporary = tempfile::tempdir().expect("temporary");
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let initial = service.inspect().expect("initial");
        service
            .initialize(&command(
                &initial,
                INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA,
            ))
            .expect("initialize");
        let path = temporary.path().join("workspace.json");
        let mut permissions = fs::metadata(&path).expect("metadata").permissions();
        make_owner_writable(&mut permissions);
        fs::set_permissions(&path, permissions).expect("writable");
        fs::write(&path, b"{}").expect("tamper");
        assert!(matches!(
            service.inspect(),
            Err(KernelError::IntegrityFailed(_))
        ));
    }

    #[test]
    fn realistic_legacy_workspace_requires_adoption_and_preserves_evidence() {
        let temporary = tempfile::tempdir().expect("temporary");
        let writer = WorkspaceWriter::open(temporary.path()).expect("legacy writer");
        writer
            .connection()
            .execute(
                "INSERT INTO jobs (
                     id, state, source_name, source_path, policy_json,
                     created_at, updated_at
                 ) VALUES (
                     'legacy-job', 'SPOOLING', 'synthetic.zip',
                     'legacy/source.zip', '{}',
                     '2026-07-29T00:00:00.000Z',
                     '2026-07-29T00:00:00.000Z'
                 )",
                [],
            )
            .expect("legacy evidence");
        drop(writer);
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let initial = service.inspect().expect("adoption state");
        assert_eq!(initial.state, ObjectWorkspaceState::AdoptRequired);
        let command = command(&initial, ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA);
        let record = service.adopt(&command).expect("adopt");
        assert_eq!(record.established_by, "ADOPTED");
        let connection =
            open_read_only(&temporary.path().join(DATABASE_FILENAME)).expect("read adopted DB");
        let source_name: String = connection
            .query_row(
                "SELECT source_name FROM jobs WHERE id = 'legacy-job'",
                [],
                |row| row.get(0),
            )
            .expect("preserved evidence");
        assert_eq!(source_name, "synthetic.zip");
    }

    #[test]
    fn stale_adoption_state_rejects_changed_legacy_evidence() {
        let temporary = tempfile::tempdir().expect("temporary");
        let writer = WorkspaceWriter::open(temporary.path()).expect("legacy writer");
        writer
            .connection()
            .execute(
                "INSERT INTO jobs (
                     id, state, source_name, source_path, policy_json,
                     created_at, updated_at
                 ) VALUES (
                     'legacy-job', 'SPOOLING', 'synthetic.zip',
                     'legacy/source.zip', '{}',
                     '2026-07-29T00:00:00.000Z',
                     '2026-07-29T00:00:00.000Z'
                 )",
                [],
            )
            .expect("legacy evidence");
        drop(writer);
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let status = service.inspect().expect("adoption state");
        let command = command(&status, ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA);
        let connection =
            Connection::open(temporary.path().join(DATABASE_FILENAME)).expect("legacy database");
        connection
            .execute(
                "UPDATE jobs SET source_name = 'changed.zip' WHERE id = 'legacy-job'",
                [],
            )
            .expect("change legacy evidence after operator read");
        drop(connection);

        assert!(matches!(
            service.adopt(&command),
            Err(KernelError::Conflict(_))
        ));
        assert!(!temporary.path().join("workspace.json").exists());
    }

    #[test]
    fn interrupted_adoption_descriptor_resumes_the_original_command() {
        let temporary = tempfile::tempdir().expect("temporary");
        let writer = WorkspaceWriter::open(temporary.path()).expect("legacy writer");
        writer
            .connection()
            .execute(
                "INSERT INTO jobs (
                     id, state, source_name, source_path, policy_json,
                     created_at, updated_at
                 ) VALUES (
                     'legacy-job', 'SPOOLING', 'synthetic.zip',
                     'legacy/source.zip', '{}',
                     '2026-07-29T00:00:00.000Z',
                     '2026-07-29T00:00:00.000Z'
                 )",
                [],
            )
            .expect("legacy evidence");
        drop(writer);
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let status = service.inspect().expect("adoption state");
        let command = command(&status, ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA);
        let canonical_request = canonical_json_string(
            &serde_json::to_value(&command).expect("canonical command value"),
        )
        .expect("canonical command");
        let request_hash = sha256_hex(canonical_request.as_bytes());
        let descriptor = new_descriptor(
            ObjectWorkspaceAction::Adopt,
            &command,
            &request_hash,
            &canonical_request,
        )
        .expect("descriptor");
        let canonical_descriptor =
            canonical_json_string(&serde_json::to_value(&descriptor).expect("descriptor JSON"))
                .expect("canonical descriptor");
        let descriptor = LoadedDescriptor {
            descriptor,
            descriptor_hash: sha256_hex(canonical_descriptor.as_bytes()),
            canonical_json: canonical_descriptor,
        };
        let lifecycle_evidence =
            capture_pending_lifecycle_evidence(temporary.path(), ObjectWorkspaceAction::Adopt)
                .expect("capture pending lifecycle evidence");
        let lock = WorkspaceLock::acquire(service.root()).expect("workspace lock");
        let writer = lock
            .open_writer_for_pending_establishment()
            .expect("pending adoption writer");
        persist_pending(
            &writer,
            &descriptor,
            ObjectWorkspaceAction::Adopt,
            &command,
            &request_hash,
            &canonical_request,
            &lifecycle_evidence,
        )
        .expect("persist pending adoption");
        drop(writer);
        drop(lock);
        publish_descriptor(temporary.path(), descriptor.canonical_json.as_bytes())
            .expect("simulate descriptor publication before interruption");

        let adopted = service
            .adopt(&command)
            .expect("resume original adoption command");
        assert_eq!(adopted.established_by, "ADOPTED");
        assert!(integrity_check(temporary.path()).expect("integrity").ok);
    }

    #[test]
    fn pending_initialization_is_pre_migration_and_rechecks_empty_evidence() {
        let temporary = tempfile::tempdir().expect("temporary");
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let status = service.inspect().expect("initial state");
        let command = command(&status, INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA);
        persist_pending_only(&service, ObjectWorkspaceAction::Initialize, &command);

        let connection =
            Connection::open(temporary.path().join(DATABASE_FILENAME)).expect("pending database");
        let user_version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("pending schema version");
        drop(connection);
        assert_eq!(
            user_version, 0,
            "pending authority must precede every application-schema migration"
        );

        fs::write(temporary.path().join("unexpected-evidence.txt"), b"changed")
            .expect("add evidence after pending commit");
        assert!(matches!(
            service.initialize(&command),
            Err(KernelError::Conflict(_))
        ));
        assert!(
            !temporary
                .path()
                .join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME)
                .exists(),
            "changed evidence must fail before descriptor publication"
        );
        fs::remove_file(temporary.path().join("unexpected-evidence.txt"))
            .expect("restore exact empty evidence");
        service
            .initialize(&command)
            .expect("unchanged exact pending command resumes");
    }

    #[test]
    fn pending_authority_cannot_be_deleted_or_replayed_without_its_exact_consume_guard() {
        let temporary = tempfile::tempdir().expect("temporary");
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let status = service.inspect().expect("initial state");
        let command = command(&status, INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA);
        persist_pending_only(&service, ObjectWorkspaceAction::Initialize, &command);

        let connection =
            Connection::open(temporary.path().join(DATABASE_FILENAME)).expect("pending database");
        connection
            .execute(
                "DELETE FROM object_workspace_pending_establishments WHERE singleton = 1",
                [],
            )
            .expect_err("pending authority must not be deletable without its exact binding");
        let pending_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM object_workspace_pending_establishments",
                [],
                |row| row.get(0),
            )
            .expect("count retained pending authority");
        assert_eq!(pending_rows, 1);
        connection
            .execute_batch("DROP TRIGGER object_workspace_pending_establishments_consume_only;")
            .expect("remove consume-only guard");
        drop(connection);

        let error = service
            .inspect()
            .expect_err("missing pending consume guard must fail replay");
        assert!(
            error
                .to_string()
                .contains("object_workspace_pending_establishments_consume_only")
        );
        let connection =
            Connection::open(temporary.path().join(DATABASE_FILENAME)).expect("pending database");
        connection
            .execute_batch(PENDING_CONSUME_ONLY_TRIGGER)
            .expect("restore exact consume-only guard");
        drop(connection);
        service
            .initialize(&command)
            .expect("exact binding may atomically consume pending authority");
    }

    #[test]
    fn pending_adoption_rechecks_legacy_file_and_sqlite_evidence() {
        let temporary = tempfile::tempdir().expect("temporary");
        let writer = WorkspaceWriter::open(temporary.path()).expect("legacy writer");
        writer
            .connection()
            .execute(
                "INSERT INTO jobs (
                     id, state, source_name, source_path, policy_json,
                     created_at, updated_at
                 ) VALUES (
                     'legacy-job', 'SPOOLING', 'synthetic.zip',
                     'legacy/source.zip', '{}',
                     '2026-07-29T00:00:00.000Z',
                     '2026-07-29T00:00:00.000Z'
                 )",
                [],
            )
            .expect("legacy row");
        drop(writer);
        let evidence_path = temporary.path().join("legacy-evidence.bin");
        fs::write(&evidence_path, b"original").expect("legacy evidence file");
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let status = service.inspect().expect("adoption state");
        let command = command(&status, ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA);
        persist_pending_only(&service, ObjectWorkspaceAction::Adopt, &command);

        fs::write(&evidence_path, b"changed!").expect("mutate legacy evidence");
        assert!(matches!(
            service.adopt(&command),
            Err(KernelError::Conflict(_))
        ));
        assert!(
            !temporary
                .path()
                .join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME)
                .exists()
        );
        fs::write(&evidence_path, b"original").expect("restore legacy evidence");

        let connection =
            Connection::open(temporary.path().join(DATABASE_FILENAME)).expect("legacy database");
        connection
            .execute(
                "UPDATE jobs SET source_name = 'changed.zip' WHERE id = 'legacy-job'",
                [],
            )
            .expect("mutate legacy SQLite evidence");
        drop(connection);
        assert!(matches!(
            service.adopt(&command),
            Err(KernelError::Conflict(_))
        ));
    }

    #[test]
    fn final_binding_commit_rechecks_evidence_and_rolls_back_on_boundary_mutation() {
        let temporary = tempfile::tempdir().expect("temporary");
        let writer = WorkspaceWriter::open(temporary.path()).expect("legacy writer");
        writer
            .connection()
            .execute(
                "INSERT INTO jobs (
                     id, state, source_name, source_path, policy_json,
                     created_at, updated_at
                 ) VALUES (
                     'legacy-final-guard-job', 'SPOOLING', 'synthetic.zip',
                     'legacy/source.zip', '{}',
                     '2026-07-29T00:00:00.000Z',
                     '2026-07-29T00:00:00.000Z'
                 )",
                [],
            )
            .expect("legacy row");
        drop(writer);
        let evidence_path = temporary.path().join("legacy-evidence.bin");
        fs::write(&evidence_path, b"original").expect("legacy evidence file");
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let status = service.inspect().expect("adoption state");
        let command = command(&status, ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA);
        *FINAL_BINDING_EVIDENCE_MUTATION
            .lock()
            .expect("final binding mutation test hook") = Some((
            service.root().to_path_buf(),
            PathBuf::from("legacy-evidence.bin"),
            b"changed!".to_vec(),
        ));

        service
            .adopt(&command)
            .expect_err("final evidence mutation must veto binding commit");
        assert!(
            !temporary
                .path()
                .join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME)
                .exists(),
            "rolled-back first attempt must remove its newly published descriptor"
        );
        let connection =
            Connection::open(temporary.path().join(DATABASE_FILENAME)).expect("pending database");
        let pending_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM object_workspace_pending_establishments",
                [],
                |row| row.get(0),
            )
            .expect("count retained pending authority");
        assert_eq!(pending_rows, 1);
        assert!(
            !table_exists(&connection, "object_workspace_bindings")
                .expect("inspect rolled-back binding table"),
            "binding DDL and row must roll back together"
        );
        drop(connection);

        fs::write(&evidence_path, b"original").expect("restore exact legacy evidence");
        let adopted = service
            .adopt(&command)
            .expect("restored exact evidence resumes the original command");
        assert_eq!(adopted.established_by, "ADOPTED");
        let report = integrity_check(temporary.path()).expect("active integrity replay");
        assert!(report.ok, "{:?}", report.issues);
    }

    #[test]
    fn descriptor_namespace_error_retains_exact_resumable_pending_authority() {
        let temporary = tempfile::tempdir().expect("temporary");
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let status = service.inspect().expect("initial state");
        let command = command(&status, INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA);
        *DESCRIPTOR_PUBLISH_FAILURE_AFTER_NAMESPACE
            .lock()
            .expect("descriptor publication test hook") = Some(service.root().to_path_buf());

        service
            .initialize(&command)
            .expect_err("injected post-publication failure");
        let interrupted = inspect_workspace(service.root()).expect("interrupted authority");
        assert!(interrupted.pending.is_some());
        assert!(interrupted.descriptor.is_some());
        let mut conflicting = command.clone();
        conflicting.idempotency_key = "workspace-command-conflict-0002".to_owned();
        assert!(matches!(
            service.initialize(&conflicting),
            Err(KernelError::Conflict(_))
        ));
        let reopened =
            ObjectWorkspaceService::new(temporary.path()).expect("reopen interrupted service");
        let pending_status = reopened.inspect().expect("inspect resumable command");
        assert_eq!(pending_status.pending_command.as_ref(), Some(&command));
        let resumed = reopened
            .initialize(
                pending_status
                    .pending_command
                    .as_ref()
                    .expect("server-returned exact pending command"),
            )
            .expect("reload resumes published descriptor with the exact durable command");
        assert_eq!(resumed.established_by, "INITIALIZED");
    }

    #[test]
    fn cleanup_only_legacy_authority_requires_explicit_adoption() {
        let temporary = tempfile::tempdir().expect("temporary");
        let writer = WorkspaceWriter::open(temporary.path()).expect("legacy writer");
        writer
            .connection()
            .execute(
                "INSERT INTO partial_cleanups (
                     cleanup_id, namespace, attempt_id, job_id, state,
                     cleanup_attempts, created_at, updated_at
                 ) VALUES (
                     'cleanup-only', 'metadata', 'metadata-only', NULL,
                     'PLANNED', 0,
                     '2026-07-29T00:00:00.000Z',
                     '2026-07-29T00:00:00.000Z'
                 )",
                [],
            )
            .expect("cleanup-only durable authority");
        drop(writer);
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let status = service.inspect().expect("lifecycle state");
        assert_eq!(status.state, ObjectWorkspaceState::AdoptRequired);
        assert!(matches!(
            establish_for_local_automation(
                temporary.path(),
                "Unsafe implicit adoption",
                "Synthetic evidence",
                "Automation"
            ),
            Err(KernelError::Conflict(_))
        ));
    }

    #[test]
    fn concurrent_initialization_has_one_identity_and_exact_replay() {
        let temporary = tempfile::tempdir().expect("temporary");
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        let initial = service.inspect().expect("initial");
        let command = command(&initial, INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA);
        let barrier = Arc::new(Barrier::new(3));
        let first_service = service.clone();
        let first_command = command.clone();
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_service.initialize(&first_command)
        });
        let second_service = service.clone();
        let second_command = command.clone();
        let second_barrier = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_service.initialize(&second_command)
        });
        barrier.wait();
        let results = [
            first.join().expect("first thread"),
            second.join().expect("second thread"),
        ];
        let active = service.require_active().expect("one active identity");
        assert!(results.iter().any(Result::is_ok));
        for result in &results {
            match result {
                Ok(record) => assert_eq!(
                    record.workspace_id, active.workspace_id,
                    "serialized exact retries must return the same identity"
                ),
                Err(error) => assert!(matches!(error, KernelError::WorkspaceLocked(_))),
            }
        }
        let replay = service.initialize(&command).expect("exact replay");
        assert_eq!(replay.workspace_id, active.workspace_id);
    }

    #[test]
    fn copied_descriptor_without_its_sqlite_ledger_is_rejected() {
        let source = tempfile::tempdir().expect("source");
        let source_service = ObjectWorkspaceService::new(source.path()).expect("source service");
        let source_status = source_service.inspect().expect("source state");
        let source_command = command(&source_status, INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA);
        source_service
            .initialize(&source_command)
            .expect("source initialize");
        let destination = tempfile::tempdir().expect("destination");
        fs::copy(
            source.path().join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME),
            destination
                .path()
                .join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME),
        )
        .expect("copy interrupted descriptor");
        let destination_service =
            ObjectWorkspaceService::new(destination.path()).expect("destination service");
        assert!(matches!(
            destination_service.inspect(),
            Err(KernelError::IntegrityFailed(_))
        ));
        assert!(
            !destination.path().join(DATABASE_FILENAME).exists(),
            "descriptor injection created SQLite authority"
        );
    }

    #[test]
    fn oversized_and_noncanonical_descriptors_fail_before_authority() {
        let oversized = tempfile::tempdir().expect("oversized");
        fs::write(
            oversized.path().join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME),
            vec![b' '; usize::try_from(MAXIMUM_DESCRIPTOR_BYTES + 1).expect("bound")],
        )
        .expect("oversized descriptor");
        let service = ObjectWorkspaceService::new(oversized.path()).expect("service");
        assert!(matches!(
            service.inspect(),
            Err(KernelError::IntegrityFailed(_))
        ));

        let noncanonical = tempfile::tempdir().expect("noncanonical");
        fs::write(
            noncanonical
                .path()
                .join(OBJECT_WORKSPACE_DESCRIPTOR_FILENAME),
            br#"{ "schemaVersion": "migration-stream/object-workspace/v1" }"#,
        )
        .expect("noncanonical descriptor");
        let service = ObjectWorkspaceService::new(noncanonical.path()).expect("service");
        assert!(matches!(
            service.inspect(),
            Err(KernelError::IntegrityFailed(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_symlink_fails_closed() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary");
        let target = temporary.path().join("target.json");
        fs::write(&target, b"{}").expect("target");
        symlink(&target, temporary.path().join("workspace.json")).expect("symlink");
        let service = ObjectWorkspaceService::new(temporary.path()).expect("service");
        assert!(matches!(
            service.inspect(),
            Err(KernelError::IntegrityFailed(_))
        ));
    }
}

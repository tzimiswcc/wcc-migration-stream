//! Platform-neutral project envelope around existing workspace authority.
//!
//! Creation publishes an immutable pending receipt. Explicit workspace
//! initialization remains the workbench's responsibility; `bind_project` then
//! publishes the final descriptor without rewriting either receipt or authority.
//! This API does not launch a runtime, import input, support adoption or promise
//! relocation. Runtime callers must still use the existing lifecycle/locks.

use crate::{
    KernelError, Result,
    canonical::parse_unique_json,
    durability::{publish_new, sync_directory},
    object_workspace::{ObjectWorkspaceService, ObjectWorkspaceState, ObjectWorkspaceStatus},
    state::{
        WorkspaceLock, integrity_check, retain_workspace_root, verify_retained_workspace_root,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const PROJECT_SCHEMA: &str = "migration-stream/project/v1";
pub const PROJECT_FILENAME: &str = "project.mstream";
pub const PENDING_PROJECT_FILENAME: &str = "project.pending.mstream";
const MAX_DESCRIPTOR_BYTES: u64 = 16 * 1024;

/// Project identity is envelope identity only; workspace identity is assigned
/// exclusively by the existing explicit workspace lifecycle.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectDescriptor {
    pub schema_version: String,
    pub project_id: String,
    pub display_name: String,
    pub workspace: String,
    pub binding: Option<ProjectBinding>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectBinding {
    pub workspace_id: String,
    pub descriptor_hash: String,
}

#[derive(Debug)]
pub struct OpenProject {
    root_handle: File,
    pub descriptor_path: PathBuf,
    pub descriptor: ProjectDescriptor,
    pub workspace: ObjectWorkspaceService,
    pub lifecycle: ObjectWorkspaceStatus,
    pub input: PathBuf,
    pub output: PathBuf,
}

/// Create a new directory and pending project, never reusing an existing root.
/// An interruption may leave an incomplete directory; it is never overwritten
/// or automatically repaired. The caller can inspect it explicitly.
///
/// # Errors
/// Fails for invalid names, existing roots, filesystem or publication errors.
pub fn create_project(root: impl AsRef<Path>, display_name: &str) -> Result<OpenProject> {
    create_project_with_parent_sync(root.as_ref(), display_name, sync_directory)
}

fn create_project_with_parent_sync(
    root: &Path,
    display_name: &str,
    sync_parent: impl FnOnce(&Path) -> std::io::Result<()>,
) -> Result<OpenProject> {
    validate_name(display_name)?;
    fs::create_dir(root).map_err(|error| KernelError::io(root, error))?;
    let parent = root
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    sync_parent(parent).map_err(|error| KernelError::io(parent, error))?;
    let retained_root = retain_workspace_root(root)?;
    for child in ["workspace", "input", "output"] {
        let path = root.join(child);
        fs::create_dir(&path).map_err(|error| KernelError::io(&path, error))?;
    }
    let descriptor = ProjectDescriptor {
        schema_version: PROJECT_SCHEMA.to_owned(),
        project_id: Uuid::new_v4().to_string(),
        display_name: display_name.to_owned(),
        workspace: "workspace".to_owned(),
        binding: None,
    };
    verify_retained_workspace_root(root, &retained_root)?;
    publish_descriptor(root, PENDING_PROJECT_FILENAME, &descriptor)?;
    verify_retained_workspace_root(root, &retained_root)?;
    open_project(root.join(PENDING_PROJECT_FILENAME))
}

/// Open and validate only. Pending projects require explicit initialization and
/// binding; bound projects require their exact active durable workspace.
///
/// # Errors
/// Rejects unsupported descriptors, missing/unsafe layout, corrupt or mismatched
/// authority and interrupted incomplete creation. Never establishes authority.
pub fn open_project(path: impl AsRef<Path>) -> Result<OpenProject> {
    let path = path.as_ref();
    let pending = match path.file_name().and_then(|value| value.to_str()) {
        Some(PENDING_PROJECT_FILENAME) => true,
        Some(PROJECT_FILENAME) => false,
        _ => return Err(invalid("unexpected project descriptor filename")),
    };
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    require_directory(parent)?;
    let root = parent
        .canonicalize()
        .map_err(|error| KernelError::io(parent, error))?;
    let root_handle = retain_workspace_root(&root)?;
    for child in ["workspace", "input", "output"] {
        require_directory(&root.join(child))?;
    }
    let descriptor_path = root.join(if pending {
        PENDING_PROJECT_FILENAME
    } else {
        PROJECT_FILENAME
    });
    let descriptor = read_descriptor(&descriptor_path)?;
    if descriptor.schema_version != PROJECT_SCHEMA
        || descriptor.workspace != "workspace"
        || Uuid::parse_str(&descriptor.project_id).is_err()
        || pending != descriptor.binding.is_none()
    {
        return Err(invalid(
            "unsupported project version, relative workspace, identity or phase",
        ));
    }
    validate_name(&descriptor.display_name)?;
    if pending {
        match fs::symlink_metadata(root.join(PROJECT_FILENAME)) {
            Ok(_) => {
                return Err(invalid(
                    "project binding already published; open project.mstream",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(KernelError::io(root.join(PROJECT_FILENAME), error)),
        }
    }
    let workspace = ObjectWorkspaceService::new(root.join("workspace"))?;
    let lifecycle = workspace.inspect()?;
    if let Some(binding) = &descriptor.binding {
        let active = workspace.require_active()?;
        if active.workspace_id != binding.workspace_id
            || active.descriptor_hash != binding.descriptor_hash
        {
            return Err(invalid(
                "project workspace identity or descriptor hash mismatch",
            ));
        }
        let report = integrity_check(workspace.root())?;
        if !report.ok {
            return Err(invalid(&format!(
                "project durable integrity: {:?}",
                report.issues
            )));
        }
    } else if lifecycle.state == ObjectWorkspaceState::AdoptRequired {
        return Err(invalid(
            "new pending project contains unexpected legacy evidence; explicit reviewed adoption required",
        ));
    }
    verify_retained_workspace_root(&root, &root_handle)?;
    Ok(OpenProject {
        root_handle,
        descriptor_path,
        descriptor,
        workspace,
        lifecycle,
        input: root.join("input"),
        output: root.join("output"),
    })
}

/// Publish the final project descriptor after explicit workspace initialization.
/// Existing final descriptors are never replaced. A published final descriptor
/// remains valid if a publication error interrupts the caller's response.
///
/// # Errors
/// Requires a pending receipt, exact active workspace, valid integrity, and the
/// existing exclusive workspace lock. No initialization/adoption occurs here.
pub fn bind_project(pending_path: impl AsRef<Path>) -> Result<OpenProject> {
    let opened = open_project(pending_path)?;
    if opened.descriptor.binding.is_some() {
        return Err(invalid("binding requires pending project receipt"));
    }
    let lock = WorkspaceLock::acquire(opened.workspace.root())?;
    let active = opened.workspace.require_active()?;
    let report = integrity_check(opened.workspace.root())?;
    if !report.ok {
        return Err(invalid("workspace integrity failed before binding"));
    }
    let mut descriptor = opened.descriptor;
    descriptor.binding = Some(ProjectBinding {
        workspace_id: active.workspace_id,
        descriptor_hash: active.descriptor_hash,
    });
    lock.verify()?;
    let root = opened
        .descriptor_path
        .parent()
        .ok_or_else(|| invalid("missing project parent"))?;
    verify_retained_workspace_root(root, &opened.root_handle)?;
    publish_descriptor(root, PROJECT_FILENAME, &descriptor)?;
    verify_retained_workspace_root(root, &opened.root_handle)?;
    lock.verify()?;
    open_project(root.join(PROJECT_FILENAME))
}

fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(invalid(
            "project name must be nonblank text up to 256 bytes without controls",
        ));
    }
    Ok(())
}

fn invalid(message: &str) -> KernelError {
    KernelError::IntegrityFailed(message.to_owned())
}

fn require_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.is_dir() || is_link(&metadata) {
        return Err(invalid("project layout requires real non-link directories"));
    }
    Ok(())
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        crate::durability::is_reparse_point(metadata)
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn read_descriptor(path: &Path) -> Result<ProjectDescriptor> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.is_file() || is_link(&metadata) || metadata.len() > MAX_DESCRIPTOR_BYTES {
        return Err(invalid(
            "project descriptor must be a bounded regular non-link file",
        ));
    }
    let file = open_descriptor_nofollow(path)?;
    let opened = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    if !opened.is_file() || is_link(&opened) || opened.len() > MAX_DESCRIPTOR_BYTES {
        return Err(invalid("opened project descriptor is unsafe or oversized"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if opened.nlink() != 1 || opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
            return Err(invalid(
                "project descriptor changed or has hard-link aliases",
            ));
        }
    }
    #[cfg(windows)]
    if !crate::durability::path_names_open_file(path, &file)
        .map_err(|error| KernelError::io(path, error))?
        || crate::durability::file_link_count(&file)
            .map_err(|error| KernelError::io(path, error))?
            != 1
    {
        return Err(invalid(
            "project descriptor changed or has hard-link aliases",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_DESCRIPTOR_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| KernelError::io(path, error))?;
    if bytes.len() as u64 > MAX_DESCRIPTOR_BYTES {
        return Err(invalid("project descriptor exceeds size bound"));
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|_| invalid("project descriptor is not UTF-8"))?;
    Ok(serde_json::from_value(parse_unique_json(text.as_bytes())?)?)
}

#[cfg(unix)]
fn open_descriptor_nofollow(path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags};
    rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|error| KernelError::io(path, std::io::Error::from(error)))
}

#[cfg(windows)]
fn open_descriptor_nofollow(path: &Path) -> Result<File> {
    crate::durability::open_file_nofollow(path).map_err(|error| KernelError::io(path, error))
}

#[cfg(not(any(unix, windows)))]
fn open_descriptor_nofollow(_path: &Path) -> Result<File> {
    Err(invalid(
        "safe descriptor opening unsupported on this platform",
    ))
}

fn publish_descriptor(root: &Path, name: &str, descriptor: &ProjectDescriptor) -> Result<()> {
    let partial = root.join(format!(".{name}.{}.partial", Uuid::new_v4()));
    let bytes = serde_json::to_vec(descriptor)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|error| KernelError::io(&partial, error))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| KernelError::io(&partial, error))?;
    drop(file);
    publish_new(&partial, &root.join(name)).map_err(|error| KernelError::io(root.join(name), error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_workspace::{
        EstablishObjectWorkspaceCommand, INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA,
    };

    fn initialize(project: &OpenProject) {
        let status = project.workspace.inspect().unwrap();
        project
            .workspace
            .initialize(&EstablishObjectWorkspaceCommand {
                schema_version: INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA.to_owned(),
                expected_state_hash: status.state_hash,
                display_name: "Synthetic object".to_owned(),
                migration_object_name_or_scope: "Test only".to_owned(),
                creator_attribution: "Synthetic test".to_owned(),
                idempotency_key: Uuid::new_v4().to_string(),
            })
            .unwrap();
    }

    fn mutate(path: &Path, key: &str, value: serde_json::Value) {
        let mut descriptor: serde_json::Value =
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        descriptor[key] = value;
        fs::write(path, serde_json::to_vec(&descriptor).unwrap()).unwrap();
    }

    #[test]
    fn create_initialize_bind_reopen_preserves_authority_and_input_is_only_a_file() {
        let temp = tempfile::tempdir().unwrap();
        let pending = create_project(temp.path().join("object"), "Customer migration").unwrap();
        assert_eq!(
            pending.lifecycle.state,
            ObjectWorkspaceState::InitializeRequired
        );
        assert!(!pending.workspace.root().join("state.sqlite3").exists());
        fs::write(pending.input.join("unaccepted.csv"), b"id\n1\n").unwrap();
        assert!(bind_project(&pending.descriptor_path).is_err());
        initialize(&pending);
        let authority = fs::read(pending.workspace.root().join("workspace.json")).unwrap();
        let bound = bind_project(&pending.descriptor_path).unwrap();
        let reopened = open_project(&bound.descriptor_path).unwrap();
        assert_eq!(bound.descriptor, reopened.descriptor);
        assert_eq!(
            authority,
            fs::read(reopened.workspace.root().join("workspace.json")).unwrap()
        );
        assert_eq!(reopened.lifecycle.state, ObjectWorkspaceState::Active);
        assert!(open_project(&pending.descriptor_path).is_err());
        assert!(bind_project(&bound.descriptor_path).is_err());
        let report = integrity_check(reopened.workspace.root()).unwrap();
        assert_eq!(report.checked_jobs, 0);
        assert!(report.ok);
    }

    #[test]
    fn parent_sync_failure_preserves_partial_root_and_prevents_overwrite() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("object");
        let error = create_project_with_parent_sync(&root, "Object", |parent| {
            assert_eq!(parent, temp.path());
            assert!(root.is_dir(), "sync must follow the new directory entry");
            assert!(fs::read_dir(&root).unwrap().next().is_none());
            Err(std::io::Error::other(
                "injected parent directory sync failure",
            ))
        })
        .unwrap_err();
        match error {
            KernelError::Io { path, source } => {
                assert_eq!(path, temp.path());
                assert_eq!(source.kind(), std::io::ErrorKind::Other);
                assert!(
                    source
                        .to_string()
                        .contains("injected parent directory sync failure")
                );
            }
            other => panic!("parent sync error must propagate: {other}"),
        }
        assert!(root.is_dir());
        assert!(fs::read_dir(&root).unwrap().next().is_none());
        assert!(open_project(root.join(PENDING_PROJECT_FILENAME)).is_err());
        assert!(create_project(&root, "Retry").is_err());
        assert!(fs::read_dir(&root).unwrap().next().is_none());
    }

    #[test]
    fn collision_and_incomplete_creation_never_overwrite_or_repair() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("object");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("keep"), b"keep").unwrap();
        assert!(create_project(&root, "Object").is_err());
        assert_eq!(fs::read(root.join("keep")).unwrap(), b"keep");
        assert!(open_project(root.join(PENDING_PROJECT_FILENAME)).is_err());
        assert!(!root.join("workspace").exists());
        assert!(create_project(temp.path().join("invalid"), "\n").is_err());
        assert!(!temp.path().join("invalid").exists());
    }

    #[test]
    fn pending_can_reopen_before_and_after_explicit_initialization_without_binding() {
        let temp = tempfile::tempdir().unwrap();
        let pending = create_project(temp.path().join("object"), "Object").unwrap();
        assert!(
            open_project(&pending.descriptor_path)
                .unwrap()
                .descriptor
                .binding
                .is_none()
        );
        initialize(&pending);
        let reopened = open_project(&pending.descriptor_path).unwrap();
        assert!(reopened.descriptor.binding.is_none());
        assert!(
            !pending
                .descriptor_path
                .parent()
                .unwrap()
                .join(PROJECT_FILENAME)
                .exists()
        );
        let lock = WorkspaceLock::acquire(pending.workspace.root()).unwrap();
        assert!(matches!(
            bind_project(&pending.descriptor_path),
            Err(KernelError::WorkspaceLocked(_))
        ));
        drop(lock);
        assert!(bind_project(&pending.descriptor_path).is_ok());
    }

    #[test]
    fn rejects_version_escape_unknown_fields_duplicate_json_and_missing_layout() {
        let temp = tempfile::tempdir().unwrap();
        let pending = create_project(temp.path().join("object"), "Object").unwrap();
        let original = fs::read(&pending.descriptor_path).unwrap();
        for value in [
            "../other",
            "/tmp",
            "C:\\workspace",
            "workspace/../workspace",
            "workspace\\child",
            "./workspace",
        ] {
            mutate(&pending.descriptor_path, "workspace", value.into());
            assert!(open_project(&pending.descriptor_path).is_err(), "{value}");
            fs::write(&pending.descriptor_path, &original).unwrap();
        }
        for (key, value) in [
            ("schemaVersion", "future"),
            ("unknownField", "surprise"),
            ("projectId", "invalid"),
        ] {
            mutate(&pending.descriptor_path, key, value.into());
            assert!(open_project(&pending.descriptor_path).is_err());
            fs::write(&pending.descriptor_path, &original).unwrap();
        }
        fs::write(
            &pending.descriptor_path,
            b"{\"schemaVersion\":1,\"schemaVersion\":2}",
        )
        .unwrap();
        assert!(open_project(&pending.descriptor_path).is_err());
        fs::write(&pending.descriptor_path, &original).unwrap();
        fs::remove_dir(&pending.output).unwrap();
        assert!(open_project(&pending.descriptor_path).is_err());
        assert!(!pending.output.exists());
    }

    #[test]
    fn distinct_objects_and_missing_mismatched_corrupt_workspace_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let a = create_project(temp.path().join("a"), "A").unwrap();
        let b = create_project(temp.path().join("b"), "B").unwrap();
        initialize(&a);
        initialize(&b);
        let a = bind_project(&a.descriptor_path).unwrap();
        let b = bind_project(&b.descriptor_path).unwrap();
        assert_ne!(a.descriptor.project_id, b.descriptor.project_id);
        assert_ne!(a.descriptor.binding, b.descriptor.binding);
        let original = fs::read(&a.descriptor_path).unwrap();
        mutate(
            &a.descriptor_path,
            "binding",
            serde_json::to_value(b.descriptor.binding).unwrap(),
        );
        assert!(open_project(&a.descriptor_path).is_err());
        fs::write(&a.descriptor_path, original).unwrap();
        let database = a.workspace.root().join("state.sqlite3");
        fs::remove_file(&database).unwrap();
        assert!(open_project(&a.descriptor_path).is_err());
        assert!(!database.exists());
        fs::write(database, b"corrupt database").unwrap();
        assert!(open_project(&a.descriptor_path).is_err());
    }

    #[test]
    fn bound_material_and_publication_collision_are_never_repaired() {
        let temp = tempfile::tempdir().unwrap();
        let pending = create_project(temp.path().join("object"), "Object").unwrap();
        initialize(&pending);
        let root = pending.descriptor_path.parent().unwrap();
        fs::write(root.join(PROJECT_FILENAME), b"collision").unwrap();
        assert!(bind_project(&pending.descriptor_path).is_err());
        assert_eq!(fs::read(root.join(PROJECT_FILENAME)).unwrap(), b"collision");
        fs::remove_file(root.join(PROJECT_FILENAME)).unwrap();
        let bound = bind_project(&pending.descriptor_path).unwrap();
        let original = fs::read(&bound.descriptor_path).unwrap();
        mutate(
            &bound.descriptor_path,
            "binding",
            serde_json::json!({
                "workspaceId": bound.descriptor.binding.as_ref().unwrap().workspace_id,
                "descriptorHash": "wrong"
            }),
        );
        assert!(open_project(&bound.descriptor_path).is_err());
        fs::write(&bound.descriptor_path, vec![b' '; 16 * 1024 + 1]).unwrap();
        assert!(open_project(&bound.descriptor_path).is_err());
        fs::write(&bound.descriptor_path, b"invalid json").unwrap();
        assert!(open_project(&bound.descriptor_path).is_err());
        fs::write(&bound.descriptor_path, original).unwrap();
        let workspace_descriptor = bound.workspace.root().join("workspace.json");
        fs::remove_file(&workspace_descriptor).unwrap();
        assert!(open_project(&bound.descriptor_path).is_err());
        assert!(!workspace_descriptor.exists());
    }

    #[cfg(unix)]
    #[test]
    fn linked_layout_and_descriptor_are_rejected() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let pending = create_project(temp.path().join("object"), "Object").unwrap();
        fs::remove_dir(&pending.input).unwrap();
        symlink(temp.path(), &pending.input).unwrap();
        assert!(open_project(&pending.descriptor_path).is_err());
        fs::remove_file(&pending.input).unwrap();
        fs::create_dir(&pending.input).unwrap();
        let saved = temp.path().join("saved.json");
        fs::rename(&pending.descriptor_path, &saved).unwrap();
        symlink(&saved, &pending.descriptor_path).unwrap();
        assert!(open_project(&pending.descriptor_path).is_err());
        fs::remove_file(&pending.descriptor_path).unwrap();
        fs::hard_link(saved, &pending.descriptor_path).unwrap();
        assert!(open_project(&pending.descriptor_path).is_err());
    }
}

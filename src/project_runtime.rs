//! Trusted bound-project selection for the existing verified-bundle runtime.
//!
//! This adapter creates no identity, session, listener or writer. Lifecycle
//! operations still require their fixed bundle controller and existing controls.

use std::{
    fs::File,
    path::{Path, PathBuf},
};

use crate::{
    KernelError, Result,
    object_workspace::ObjectWorkspaceState,
    project_object::{OpenProject, PENDING_PROJECT_FILENAME, open_project},
    runtime_lifecycle::{
        RuntimeCommandOptions, RuntimeSelection, RuntimeWorkspaceIdentity, selection_sealed,
    },
    state::{retain_workspace_root, verify_retained_workspace_root},
};

/// A trusted caller's exact already-bound descriptor and retained directory.
/// Recreate this selection when deliberately choosing a different project.
#[derive(Debug)]
pub struct BoundProjectRuntime {
    project: OpenProject,
    root_handle: File,
    expected_workspace: RuntimeWorkspaceIdentity,
    options: RuntimeCommandOptions,
}

impl BoundProjectRuntime {
    /// Resolve a bound descriptor without establishing, adopting or approving.
    ///
    /// # Errors
    /// Pending descriptors require explicit workspace initialization/binding;
    /// invalid descriptors, inactive authority and unsafe paths are rejected.
    pub fn open(path: impl AsRef<Path>, port: u16) -> Result<Self> {
        let path = path.as_ref();
        if path
            .file_name()
            .is_some_and(|name| name == PENDING_PROJECT_FILENAME)
        {
            return Err(conflict(
                "pending project requires explicit workspace initialization and bind_project before runtime commands; open the final project.mstream after binding",
            ));
        }
        if port == 0 {
            return Err(conflict(
                "project runtime requires an explicit non-zero loopback port",
            ));
        }
        let project = open_project(path)?;
        if project.descriptor.binding.is_none() {
            return Err(conflict(
                "initialize the workspace explicitly, then bind_project before runtime commands",
            ));
        }
        let root = project.workspace.root();
        let root_handle = retain_workspace_root(root)?;
        let active = project.workspace.require_active()?;
        verify_retained_workspace_root(root, &root_handle)?;
        let (canonical_path, filesystem_identity_sha256) =
            native_workspace_identity(root, &root_handle)?;
        #[cfg(windows)]
        verify_retained_workspace_root(root, &root_handle)?;
        let expected_workspace = RuntimeWorkspaceIdentity {
            canonical_path,
            filesystem_identity_sha256,
            object_workspace_state: ObjectWorkspaceState::Active,
            object_workspace: Some(active),
        };
        let options = RuntimeCommandOptions {
            workspace: root.to_path_buf(),
            port,
        };
        Ok(Self {
            project,
            root_handle,
            expected_workspace,
            options,
        })
    }

    pub fn project_path(&self) -> &Path {
        &self.project.descriptor_path
    }

    pub fn expected_workspace(&self) -> &RuntimeWorkspaceIdentity {
        &self.expected_workspace
    }
}

impl selection_sealed::Sealed for BoundProjectRuntime {}

impl RuntimeSelection for BoundProjectRuntime {
    fn runtime_options(&self) -> &RuntimeCommandOptions {
        &self.options
    }

    fn validate_runtime_workspace(&self, actual: &RuntimeWorkspaceIdentity) -> Result<()> {
        verify_retained_workspace_root(self.project.workspace.root(), &self.root_handle)?;
        self.project.workspace.require_active()?;
        let reopened = open_project(&self.project.descriptor_path)?;
        if reopened.descriptor != self.project.descriptor {
            return Err(conflict("bound project descriptor changed after selection"));
        }
        if actual != &self.expected_workspace {
            return Err(conflict(
                "runtime workspace differs from the selected project's exact native identity or binding",
            ));
        }
        verify_retained_workspace_root(self.project.workspace.root(), &self.root_handle)
    }
}

fn conflict(message: &str) -> KernelError {
    KernelError::RuntimeConflict(message.to_owned())
}

#[cfg(unix)]
fn native_workspace_identity(root: &Path, file: &File) -> Result<(PathBuf, String)> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io("selected project directory handle", error))?;
    Ok((
        root.to_path_buf(),
        crate::runtime_lifecycle::unix_workspace_filesystem_identity_sha256(
            metadata.dev(),
            metadata.ino(),
        ),
    ))
}

#[cfg(windows)]
fn native_workspace_identity(root: &Path, file: &File) -> Result<(PathBuf, String)> {
    use migration_stream_windows_native::{
        RealDirectory, file_identity_from_open_file, windows_workspace_filesystem_identity_sha256,
    };
    // Match the lifecycle's GUID-volume location without trusting a path spelling.
    // The capture holds no-follow directory handles while both fields are read.
    let directory = RealDirectory::capture(root).map_err(|error| KernelError::io(root, error))?;
    let retained_identity = file_identity_from_open_file(file)
        .map_err(|error| KernelError::io("selected project directory handle", error))?;
    directory
        .verify()
        .map_err(|error| KernelError::io(root, error))?;
    let identity = directory.identity();
    if identity != retained_identity {
        return Err(conflict(
            "selected project directory capture differs from its retained identity",
        ));
    }
    Ok((
        directory.path().to_path_buf(),
        windows_workspace_filesystem_identity_sha256(identity),
    ))
}

#[cfg(not(any(unix, windows)))]
fn native_workspace_identity(_root: &Path, _file: &File) -> Result<(PathBuf, String)> {
    Err(KernelError::BundlePlatform(
        "project runtime native directory identity is unavailable on this target".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        object_workspace::establish_for_local_automation,
        project_object::{bind_project, create_project},
    };

    fn bound(root: &Path) -> OpenProject {
        let pending = create_project(root, "Runtime adapter synthetic object").unwrap();
        establish_for_local_automation(
            pending.workspace.root(),
            "Runtime adapter",
            "Synthetic local object",
            "Integration proof",
        )
        .unwrap();
        bind_project(pending.descriptor_path).unwrap()
    }

    #[test]
    fn pending_requires_explicit_initialization_and_binding_even_after_establishment() {
        let temp = tempfile::tempdir().unwrap();
        let pending = create_project(temp.path().join("pending"), "Pending object").unwrap();
        let error = BoundProjectRuntime::open(&pending.descriptor_path, 4173).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("initialization and bind_project")
        );
        assert!(!pending.workspace.root().join("state.sqlite3").exists());
        establish_for_local_automation(pending.workspace.root(), "Pending", "Synthetic", "Proof")
            .unwrap();
        assert!(BoundProjectRuntime::open(&pending.descriptor_path, 4173).is_err());
        assert!(
            !pending
                .descriptor_path
                .parent()
                .unwrap()
                .join("project.mstream")
                .exists()
        );
    }

    #[test]
    fn selection_pins_descriptor_binding_and_native_identity_before_runtime_control() {
        let temp = tempfile::tempdir().unwrap();
        let project = bound(&temp.path().join("a"));
        let selection = BoundProjectRuntime::open(&project.descriptor_path, 4173).unwrap();
        assert_eq!(
            selection.runtime_options().workspace,
            project.workspace.root()
        );
        selection
            .validate_runtime_workspace(selection.expected_workspace())
            .unwrap();
        let mut wrong = selection.expected_workspace().clone();
        wrong.filesystem_identity_sha256 = "0".repeat(64);
        assert!(selection.validate_runtime_workspace(&wrong).is_err());
        let other = bound(&temp.path().join("b"));
        let mut altered = project.descriptor;
        altered.binding = other.descriptor.binding;
        std::fs::write(
            &project.descriptor_path,
            serde_json::to_vec(&altered).unwrap(),
        )
        .unwrap();
        assert!(
            selection
                .validate_runtime_workspace(selection.expected_workspace())
                .is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_selection_matches_independent_native_runtime_capture() {
        use crate::object_workspace::ObjectWorkspaceService;
        use migration_stream_windows_native::{
            RealDirectory, windows_workspace_filesystem_identity_sha256,
        };

        let temp = tempfile::tempdir().unwrap();
        let project = bound(&temp.path().join("native-selection"));
        let selection = BoundProjectRuntime::open(&project.descriptor_path, 4173).unwrap();
        // Independently reproduce the Windows lifecycle capture, rather than
        // validating expected_workspace against itself as the older test did.
        let directory = RealDirectory::capture(project.workspace.root()).unwrap();
        let status = ObjectWorkspaceService::new(directory.path())
            .unwrap()
            .inspect()
            .unwrap();
        directory.verify().unwrap();
        let actual = RuntimeWorkspaceIdentity {
            canonical_path: directory.path().to_path_buf(),
            filesystem_identity_sha256: windows_workspace_filesystem_identity_sha256(
                directory.identity(),
            ),
            object_workspace_state: status.state,
            object_workspace: status.workspace,
        };
        assert_eq!(selection.expected_workspace(), &actual);
        selection.validate_runtime_workspace(&actual).unwrap();

        let other = bound(&temp.path().join("other-native-selection"));
        let other_handle = retain_workspace_root(other.workspace.root()).unwrap();
        assert!(native_workspace_identity(project.workspace.root(), &other_handle).is_err());
        let mut wrong = actual.clone();
        wrong.canonical_path = wrong.canonical_path.join("different-location");
        assert!(selection.validate_runtime_workspace(&wrong).is_err());
        wrong = actual.clone();
        wrong.filesystem_identity_sha256 = "0".repeat(64);
        assert!(selection.validate_runtime_workspace(&wrong).is_err());
        wrong = actual.clone();
        wrong.object_workspace.as_mut().unwrap().descriptor_hash = "0".repeat(64);
        assert!(selection.validate_runtime_workspace(&wrong).is_err());
        directory.verify().unwrap();
    }

    #[test]
    fn retained_selection_rejects_a_replaced_workspace_root() {
        let temp = tempfile::tempdir().unwrap();
        let project = bound(&temp.path().join("a"));
        let selection = BoundProjectRuntime::open(&project.descriptor_path, 4173).unwrap();
        let root = project.workspace.root();
        std::fs::rename(root, temp.path().join("displaced-workspace")).unwrap();
        std::fs::create_dir(root).unwrap();
        assert!(
            selection
                .validate_runtime_workspace(selection.expected_workspace())
                .is_err()
        );
        assert!(!root.join("state.sqlite3").exists());
    }
}

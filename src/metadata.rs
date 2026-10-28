//! Workspace orchestration for immutable technical-metadata evidence.
//!
//! The workspace advisory lock spans snapshot preservation, catalogue
//! preservation, and `SQLite` registration. A crash may leave an unreferenced
//! content-addressed file, but it cannot create a false successful parser run.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Read, Seek, Write},
    path::{Path, PathBuf},
};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    KernelError, Result,
    durability::{path_uses_tombstone_namespace, publish_new},
    edmx::{EdmxLimits, EdmxSourceContext, import_edmx_file},
    state::{
        CleanupNamespace, MetadataImportRegistration, MetadataSnapshotRegistration,
        ParserRunRegistration, WorkspaceWriter, recover_namespace_partials,
        register_metadata_import_locked,
    },
};

/// Durable result of one offline EDMX import.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataImportReport {
    pub snapshot_id: String,
    pub status: &'static str,
    pub snapshot_path: PathBuf,
    pub snapshot_sha256: String,
    pub snapshot_bytes: u64,
    pub catalogue_path: PathBuf,
    pub catalogue_file_sha256: String,
    pub catalogue_payload_sha256: String,
    pub parser_name: String,
    pub parser_version: String,
    pub imported_at: String,
}

/// Preserve an EDMX snapshot, create a deterministic technical catalogue, and
/// atomically register their evidence in `SQLite` while holding the workspace writer lock.
///
/// # Errors
///
/// Returns a typed workspace, I/O, EDMX, JSON, or `SQLite` error. Parser output is
/// always `GENERATED_DRAFT`; this function cannot approve a profile.
pub fn import_edmx_to_workspace(
    source: &Path,
    workspace: &Path,
    context: EdmxSourceContext,
    limits: &EdmxLimits,
) -> Result<MetadataImportReport> {
    if path_uses_tombstone_namespace(source) {
        return Err(KernelError::QuarantinedSource(source.to_path_buf()));
    }
    let writer = WorkspaceWriter::open(workspace)?;
    clean_stale_partials(&writer)?;
    let started_at = now();
    let system_context = context.system.clone();
    let client_context = context.client.clone();
    let release_context = context.release.clone();
    let imported = import_edmx_file(source, context, limits)?;
    imported.verify_evidence()?;

    let snapshot_hash = imported.catalogue.snapshot.sha256.clone();
    let snapshot_bytes = imported.catalogue.snapshot.byte_length;
    let snapshot_path = preserve_snapshot(source, &writer, &snapshot_hash, snapshot_bytes)?;
    let catalogue_bytes = imported.to_pretty_json()?;
    let (catalogue_path, catalogue_file_hash) = preserve_bytes(
        &writer,
        CleanupNamespace::Catalogues,
        "json",
        &catalogue_bytes,
    )?;
    let completed_at = now();
    let snapshot_id = deterministic_snapshot_id(&imported.catalogue.snapshot)?;
    let attempted_imported_at = completed_at.clone();
    let relative_snapshot = relative_evidence_path(&writer.root, &snapshot_path)?;
    let relative_catalogue = relative_evidence_path(&writer.root, &catalogue_path)?;
    let parser = &imported.catalogue.parser;
    let registration = MetadataImportRegistration {
        snapshot: MetadataSnapshotRegistration {
            id: snapshot_id.clone(),
            artifact_name: imported.catalogue.snapshot.context.artifact.clone(),
            immutable_path: relative_snapshot,
            sha256: snapshot_hash.clone(),
            byte_size: snapshot_bytes,
            system_context,
            client_context,
            release_context,
            imported_at: attempted_imported_at,
        },
        parser_run: ParserRunRegistration {
            parser_name: parser.name.clone(),
            parser_version: parser.version.clone(),
            options: serde_json::to_value(&parser.options)?,
            outcome: "SUCCEEDED_GENERATED_DRAFT".to_owned(),
            catalogue_path: Some(relative_catalogue),
            catalogue_sha256: Some(catalogue_file_hash.clone()),
            evidence: serde_json::to_value(&imported.evidence)?,
            started_at,
            completed_at,
        },
    };
    let imported_at = register_metadata_import_locked(&writer, &registration)?;

    Ok(MetadataImportReport {
        snapshot_id,
        status: "GENERATED_DRAFT",
        snapshot_path,
        snapshot_sha256: snapshot_hash,
        snapshot_bytes,
        catalogue_path,
        catalogue_file_sha256: catalogue_file_hash,
        catalogue_payload_sha256: imported.evidence.sha256.clone(),
        parser_name: parser.name.clone(),
        parser_version: parser.version.clone(),
        imported_at,
    })
}

fn deterministic_snapshot_id(snapshot: &crate::edmx::EdmxSnapshotEvidence) -> Result<String> {
    let identity = serde_json::to_vec(snapshot)?;
    Ok(format!(
        "edmx-{}",
        crate::canonical::lower_hex(Sha256::digest(identity))
    ))
}

fn preserve_snapshot(
    source: &Path,
    writer: &WorkspaceWriter,
    expected_hash: &str,
    expected_size: u64,
) -> Result<PathBuf> {
    let workspace = &writer.root;
    let metadata = fs::symlink_metadata(source).map_err(|error| KernelError::io(source, error))?;
    let source_is_regular = metadata.file_type().is_file();
    #[cfg(windows)]
    let source_is_regular = source_is_regular && !crate::durability::is_reparse_point(&metadata);
    if !source_is_regular {
        return Err(KernelError::SourceNotRegular(source.to_path_buf()));
    }
    let directory = ensure_evidence_directory(workspace, "metadata")?;
    let destination = directory.join(format!("{expected_hash}.edmx"));
    match fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            let destination_is_regular = metadata.file_type().is_file();
            #[cfg(windows)]
            let destination_is_regular =
                destination_is_regular && !crate::durability::is_reparse_point(&metadata);
            if !destination_is_regular {
                return Err(KernelError::EdmxParse(format!(
                    "existing snapshot {} is not a regular file",
                    destination.display()
                )));
            }
            verify_readonly_evidence(&destination, expected_hash, expected_size, "snapshot")?;
            return Ok(destination);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(KernelError::io(&destination, error)),
    }

    let partial = directory.join(format!("{}.part", Uuid::new_v4()));
    let copy_result: Result<()> = (|| {
        let input = File::open(source).map_err(|error| KernelError::io(source, error))?;
        let mut input = BufReader::with_capacity(64 * 1024, input);
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&partial)
            .map_err(|error| KernelError::io(&partial, error))?;
        let mut digest = Sha256::new();
        let mut copied_size = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = input
                .read(&mut buffer)
                .map_err(|error| KernelError::io(source, error))?;
            if read == 0 {
                break;
            }
            output
                .write_all(&buffer[..read])
                .map_err(|error| KernelError::io(&partial, error))?;
            digest.update(&buffer[..read]);
            copied_size = copied_size
                .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
                .ok_or_else(|| KernelError::EdmxLimit("snapshot byte count overflow".to_owned()))?;
        }
        output
            .sync_all()
            .map_err(|error| KernelError::io(&partial, error))?;
        let actual_hash = crate::canonical::lower_hex(digest.finalize());
        if actual_hash != expected_hash || copied_size != expected_size {
            return Err(KernelError::EdmxParse(format!(
                "snapshot changed between parse and immutable promotion: expected {expected_hash}/{expected_size}, found {actual_hash}/{copied_size}"
            )));
        }
        make_handle_readonly(&output, &partial)?;
        verify_named_file_identity(&partial, &output)?;
        drop(output);
        promote_readonly_partial(
            writer,
            CleanupNamespace::Metadata,
            &partial,
            &destination,
            expected_hash,
            expected_size,
            "snapshot",
        )?;
        Ok(())
    })();
    if let Err(error) = copy_result {
        return Err(failed_partial_error(
            writer,
            CleanupNamespace::Metadata,
            error,
        ));
    }
    Ok(destination)
}

fn preserve_bytes(
    writer: &WorkspaceWriter,
    namespace: CleanupNamespace,
    extension: &str,
    bytes: &[u8],
) -> Result<(PathBuf, String)> {
    let workspace = &writer.root;
    let hash = crate::canonical::lower_hex(Sha256::digest(bytes));
    let directory = ensure_evidence_directory(workspace, namespace.as_str())?;
    let destination = directory.join(format!("{hash}.{extension}"));
    match fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            let destination_is_regular = metadata.file_type().is_file();
            #[cfg(windows)]
            let destination_is_regular =
                destination_is_regular && !crate::durability::is_reparse_point(&metadata);
            if !destination_is_regular {
                return Err(KernelError::EdmxParse(format!(
                    "existing catalogue {} is not a regular file",
                    destination.display()
                )));
            }
            verify_readonly_evidence(
                &destination,
                &hash,
                u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                "catalogue",
            )?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let partial = directory.join(format!("{}.part", Uuid::new_v4()));
            let write_result: Result<()> = (|| {
                let mut output = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&partial)
                    .map_err(|error| KernelError::io(&partial, error))?;
                output
                    .write_all(bytes)
                    .map_err(|error| KernelError::io(&partial, error))?;
                output
                    .sync_all()
                    .map_err(|error| KernelError::io(&partial, error))?;
                make_handle_readonly(&output, &partial)?;
                verify_named_file_identity(&partial, &output)?;
                drop(output);
                promote_readonly_partial(
                    writer,
                    namespace,
                    &partial,
                    &destination,
                    &hash,
                    u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                    "catalogue",
                )?;
                Ok(())
            })();
            if let Err(error) = write_result {
                return Err(failed_partial_error(writer, namespace, error));
            }
        }
        Err(error) => return Err(KernelError::io(&destination, error)),
    }
    Ok((destination, hash))
}

fn clean_stale_partials(writer: &WorkspaceWriter) -> Result<()> {
    for namespace in [CleanupNamespace::Metadata, CleanupNamespace::Catalogues] {
        ensure_evidence_directory(&writer.root, namespace.as_str())?;
        let cleanup = recover_namespace_partials(writer, namespace)?;
        cleanup.ensure_no_pending()?;
    }
    Ok(())
}

fn failed_partial_error(
    writer: &WorkspaceWriter,
    namespace: CleanupNamespace,
    original: KernelError,
) -> KernelError {
    match recover_namespace_partials(writer, namespace) {
        Ok(cleanup) => match cleanup.ensure_no_pending() {
            Ok(()) => original,
            Err(cleanup_error) => KernelError::Reconciliation(format!(
                "metadata evidence operation failed: {original}; live partial was quarantined but cleanup remains pending: {cleanup_error}"
            )),
        },
        Err(cleanup_error) => KernelError::Reconciliation(format!(
            "metadata evidence operation failed: {original}; partial recovery also failed: {cleanup_error}"
        )),
    }
}

fn ensure_evidence_directory(workspace: &Path, directory_name: &str) -> Result<PathBuf> {
    let directory = workspace.join(directory_name);
    fs::create_dir_all(&directory).map_err(|error| KernelError::io(&directory, error))?;
    let metadata =
        fs::symlink_metadata(&directory).map_err(|error| KernelError::io(&directory, error))?;
    let is_real_directory = metadata.file_type().is_dir();
    #[cfg(windows)]
    let is_real_directory = is_real_directory && !crate::durability::is_reparse_point(&metadata);
    if !is_real_directory {
        return Err(KernelError::EdmxParse(format!(
            "evidence directory {} is not a real directory",
            directory.display()
        )));
    }
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|error| KernelError::io(workspace, error))?;
    let canonical_directory = directory
        .canonicalize()
        .map_err(|error| KernelError::io(&directory, error))?;
    if !canonical_directory.starts_with(&canonical_workspace) {
        return Err(KernelError::EdmxParse(format!(
            "evidence directory {} resolves outside workspace {}",
            directory.display(),
            workspace.display()
        )));
    }
    Ok(directory)
}

fn promote_readonly_partial(
    writer: &WorkspaceWriter,
    namespace: CleanupNamespace,
    partial: &Path,
    destination: &Path,
    expected_hash: &str,
    expected_size: u64,
    label: &str,
) -> Result<()> {
    match publish_new(partial, destination) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            verify_readonly_evidence(destination, expected_hash, expected_size, label)?;
            let cleanup = recover_namespace_partials(writer, namespace)?;
            cleanup.ensure_no_pending()?;
            return Ok(());
        }
        Err(error) => return Err(KernelError::io(destination, error)),
    }
    verify_readonly_evidence(destination, expected_hash, expected_size, label)?;
    Ok(())
}

fn verify_readonly_evidence(
    path: &Path,
    expected_hash: &str,
    expected_size: u64,
    label: &str,
) -> Result<()> {
    let file = open_regular_nofollow(path)?;
    let (actual_hash, actual_size) = hash_file_handle(&file, path)?;
    if actual_hash != expected_hash || actual_size != expected_size {
        return Err(KernelError::EdmxParse(format!(
            "existing {label} {} does not match its content-addressed hash/size: expected {expected_hash}/{expected_size}, found {actual_hash}/{actual_size}",
            path.display()
        )));
    }
    #[cfg(windows)]
    {
        if !file
            .metadata()
            .map_err(|error| KernelError::io(path, error))?
            .permissions()
            .readonly()
        {
            make_handle_readonly(&file, path)?;
        }
        if !file
            .metadata()
            .map_err(|error| KernelError::io(path, error))?
            .permissions()
            .readonly()
        {
            return Err(KernelError::EdmxParse(format!(
                "existing {label} {} could not be made read-only",
                path.display()
            )));
        }
    }
    #[cfg(not(windows))]
    make_handle_readonly(&file, path)?;
    verify_named_file_identity(path, &file)?;
    Ok(())
}

fn hash_file_handle(file: &File, path: &Path) -> Result<(String, u64)> {
    let mut input = file
        .try_clone()
        .map_err(|error| KernelError::io(path, error))?;
    input
        .rewind()
        .map_err(|error| KernelError::io(path, error))?;
    let mut input = BufReader::with_capacity(64 * 1024, input);
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|error| KernelError::io(path, error))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        size = size
            .checked_add(u64::try_from(read).unwrap_or(u64::MAX))
            .ok_or_else(|| KernelError::EdmxLimit("snapshot byte count overflow".to_owned()))?;
    }
    Ok((crate::canonical::lower_hex(digest.finalize()), size))
}

#[cfg(unix)]
fn open_regular_nofollow(path: &Path) -> Result<File> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| map_regular_open_error(path, error.into()))?;
    let file = File::from(descriptor);
    if !file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?
        .is_file()
    {
        return Err(KernelError::SourceNotRegular(path.to_path_buf()));
    }
    Ok(file)
}

#[cfg(windows)]
fn open_regular_nofollow(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.file_type().is_file() || crate::durability::is_reparse_point(&metadata) {
        return Err(KernelError::SourceNotRegular(path.to_path_buf()));
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(!metadata.permissions().readonly())
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options
        .open(path)
        .map_err(|error| KernelError::io(path, error))?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    if !opened_metadata.is_file() || crate::durability::is_reparse_point(&opened_metadata) {
        return Err(KernelError::SourceNotRegular(path.to_path_buf()));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_regular_nofollow(path: &Path) -> Result<File> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.file_type().is_file() {
        return Err(KernelError::SourceNotRegular(path.to_path_buf()));
    }
    let file = File::open(path).map_err(|error| KernelError::io(path, error))?;
    if !file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?
        .is_file()
    {
        return Err(KernelError::SourceNotRegular(path.to_path_buf()));
    }
    Ok(file)
}

#[cfg(unix)]
fn map_regular_open_error(path: &Path, error: io::Error) -> KernelError {
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.file_type().is_file()) {
        KernelError::SourceNotRegular(path.to_path_buf())
    } else {
        KernelError::io(path, error)
    }
}

fn make_handle_readonly(file: &File, path: &Path) -> Result<()> {
    let mut permissions = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?
        .permissions();
    permissions.set_readonly(true);
    file.set_permissions(permissions)
        .map_err(|error| KernelError::io(path, error))?;
    file.sync_all()
        .map_err(|error| KernelError::io(path, error))
}

fn verify_named_file_identity(path: &Path, file: &File) -> Result<()> {
    let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    let is_regular = named.file_type().is_file();
    #[cfg(windows)]
    let is_regular = is_regular && !crate::durability::is_reparse_point(&named);
    if !is_regular {
        return Err(KernelError::EdmxParse(format!(
            "evidence path {} is no longer a regular file",
            path.display()
        )));
    }
    #[cfg(not(windows))]
    let opened = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if named.dev() != opened.dev() || named.ino() != opened.ino() {
            return Err(KernelError::EdmxParse(format!(
                "evidence path {} no longer names the verified file",
                path.display()
            )));
        }
    }
    #[cfg(windows)]
    if !crate::durability::path_names_open_file(path, file)
        .map_err(|error| KernelError::io(path, error))?
    {
        return Err(KernelError::EdmxParse(format!(
            "evidence path {} no longer names the verified file",
            path.display()
        )));
    }
    #[cfg(not(any(unix, windows)))]
    if named.len() != opened.len() {
        return Err(KernelError::EdmxParse(format!(
            "evidence path {} changed during verification",
            path.display()
        )));
    }
    Ok(())
}

fn relative_evidence_path(workspace: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(workspace).map_err(|_| {
        KernelError::EdmxParse(format!(
            "evidence path {} is not contained by workspace {}",
            path.display(),
            workspace.display()
        ))
    })?;
    relative
        .components()
        .map(|component| {
            let component = component.as_os_str().to_str().ok_or_else(|| {
                KernelError::EdmxParse(format!(
                    "evidence path {} contains a non-Unicode component",
                    path.display()
                ))
            })?;
            if component.is_empty() || matches!(component, "." | "..") {
                return Err(KernelError::EdmxParse(format!(
                    "evidence path {} contains an unsafe relative component",
                    path.display()
                )));
            }
            Ok(component)
        })
        .collect::<Result<Vec<_>>>()
        .map(|components| components.join("/"))
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

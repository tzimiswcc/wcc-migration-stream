//! Platform-specific durable namespace transitions.

use std::{fs, io, path::Path};

pub(crate) const TOMBSTONE_EXTENSION: &str = "tombstone";

/// Publish a fully synchronized sibling partial without replacing an existing
/// destination.
pub(crate) fn publish_new(partial: &Path, destination: &Path) -> io::Result<()> {
    let parent = common_parent(partial, destination)?;
    publish_new_platform(partial, destination, parent)
}

#[cfg(unix)]
fn publish_new_platform(partial: &Path, destination: &Path, parent: &Path) -> io::Result<()> {
    fs::hard_link(partial, destination)?;
    fs::remove_file(partial)?;
    sync_directory(parent)
}

#[cfg(windows)]
fn publish_new_platform(partial: &Path, destination: &Path, _parent: &Path) -> io::Result<()> {
    atomicwrites::move_atomic(partial, destination)
}

#[cfg(not(any(unix, windows)))]
fn publish_new_platform(_partial: &Path, _destination: &Path, _parent: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "durable no-replace publication is not implemented for this platform",
    ))
}

/// Perform the Windows live-part to inert-tombstone transition. The
/// `atomicwrites` Windows implementation calls `MoveFileExW` with
/// `MOVEFILE_WRITE_THROUGH` only: it neither replaces nor enables copy fallback.
#[cfg(windows)]
pub(crate) fn quarantine_write_through(partial: &Path, tombstone: &Path) -> io::Result<()> {
    let _parent = common_parent(partial, tombstone)?;
    atomicwrites::move_atomic(partial, tombstone)
}

pub(crate) fn path_uses_tombstone_namespace(path: &Path) -> bool {
    path.components().any(|component| match component {
        std::path::Component::Normal(component) => component_uses_tombstone_namespace(component),
        _ => false,
    }) || path.canonicalize().is_ok_and(|canonical| {
        canonical.components().any(|component| match component {
            std::path::Component::Normal(component) => {
                component_uses_tombstone_namespace(component)
            }
            _ => false,
        })
    })
}

fn component_uses_tombstone_namespace(component: &std::ffi::OsStr) -> bool {
    let component = component.to_string_lossy();
    #[cfg(windows)]
    {
        let component = component
            .split_once(':')
            .map_or(component.as_ref(), |(base, _stream)| base)
            .trim_end_matches(['.', ' ']);
        component
            .get(
                component
                    .len()
                    .saturating_sub(TOMBSTONE_EXTENSION.len() + 1)..,
            )
            .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".tombstone"))
    }
    #[cfg(not(windows))]
    {
        component.ends_with(".tombstone")
    }
}

#[cfg(windows)]
pub(crate) fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(windows)]
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
#[cfg(windows)]
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

#[cfg(windows)]
pub(crate) fn open_file_nofollow(path: &Path) -> io::Result<fs::File> {
    open_path_nofollow(path, false)
}

#[cfg(windows)]
pub(crate) fn open_directory_nofollow(path: &Path) -> io::Result<fs::File> {
    open_path_nofollow(path, true)
}

#[cfg(windows)]
fn open_path_nofollow(path: &Path, directory: bool) -> io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    let flags = if directory {
        FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS
    } else {
        FILE_FLAG_OPEN_REPARSE_POINT
    };
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
}

/// Verify that a named Windows path denotes a retained open file without
/// following a reparse point. Attributes and identity are both read from the
/// same handle so a concurrent name replacement cannot create a mixed view.
#[cfg(windows)]
pub(crate) fn path_names_open_file(path: &Path, file: &fs::File) -> io::Result<bool> {
    path_names_open_path(path, file, false)
}

/// Verify that a named Windows path denotes a retained open directory without
/// following a reparse point.
#[cfg(windows)]
pub(crate) fn path_names_open_directory(path: &Path, file: &fs::File) -> io::Result<bool> {
    path_names_open_path(path, file, true)
}

#[cfg(windows)]
pub(crate) fn file_link_count(file: &fs::File) -> io::Result<u64> {
    winapi_util::file::information(file).map(|information| information.number_of_links())
}

#[cfg(windows)]
fn path_names_open_path(path: &Path, file: &fs::File, directory: bool) -> io::Result<bool> {
    let named_file = open_path_nofollow(path, directory)?;
    let named_metadata = named_file.metadata()?;
    let expected_kind = if directory {
        named_metadata.file_type().is_dir()
    } else {
        named_metadata.file_type().is_file()
    };
    if !expected_kind || is_reparse_point(&named_metadata) {
        return Ok(false);
    }
    let opened = same_file::Handle::from_file(file.try_clone()?)?;
    let named = same_file::Handle::from_file(named_file)?;
    Ok(opened == named)
}

fn common_parent<'a>(source: &'a Path, destination: &Path) -> io::Result<&'a Path> {
    let source_parent = required_parent(source)?;
    let destination_parent = required_parent(destination)?;
    if source_parent != destination_parent {
        return Err(io::Error::new(
            io::ErrorKind::CrossesDevices,
            "durable namespace transition requires the same parent directory",
        ));
    }
    Ok(source_parent)
}

fn required_parent(path: &Path) -> io::Result<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path {} has no usable parent", path.display()),
            )
        })
}

/// Synchronize a directory's namespace on Unix. Other platforms currently have
/// no equivalent in this portability layer; successful return there is not a
/// directory-entry crash-durability guarantee.
#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path).and_then(|directory| directory.sync_all())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn sync_directory(_path: &Path) -> io::Result<()> {
    // Match the existing workspace/bundle directory-sync portability policy.
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn publication_never_replaces_an_existing_name() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let partial = temporary.path().join("source.part");
        let destination = temporary.path().join("destination.bin");
        fs::write(&partial, b"new").expect("partial");
        fs::write(&destination, b"existing").expect("destination");

        let error = publish_new(&partial, &destination).expect_err("must not replace");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&destination).expect("destination"), b"existing");
        assert_eq!(fs::read(&partial).expect("partial"), b"new");
    }

    #[test]
    fn publication_moves_a_synchronized_sibling_partial() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let partial = temporary.path().join("source.part");
        let destination = temporary.path().join("destination.bin");
        let mut file = fs::File::create(&partial).expect("partial");
        file.write_all(b"payload").expect("payload");
        file.sync_all().expect("sync partial");
        drop(file);

        publish_new(&partial, &destination).expect("publish");
        assert!(!partial.exists());
        assert_eq!(fs::read(&destination).expect("destination"), b"payload");
    }

    #[test]
    fn publication_rejects_a_different_parent_before_moving() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        fs::create_dir_all(&first).expect("first");
        fs::create_dir_all(&second).expect("second");
        let partial = first.join("source.part");
        let destination = second.join("destination.bin");
        fs::write(&partial, b"payload").expect("partial");

        let error = publish_new(&partial, &destination).expect_err("different parent");
        assert_eq!(error.kind(), io::ErrorKind::CrossesDevices);
        assert!(partial.exists());
        assert!(!destination.exists());
    }

    #[test]
    fn tombstone_namespace_is_reserved_for_direct_and_canonical_paths() {
        let direct = Path::new("workspace/packages/id.tombstone");
        assert!(path_uses_tombstone_namespace(direct));
        assert!(!path_uses_tombstone_namespace(Path::new(
            "workspace/packages/source.zip"
        )));
    }

    #[cfg(windows)]
    #[test]
    fn windows_tombstone_aliases_are_reserved() {
        assert!(path_uses_tombstone_namespace(Path::new(
            r"C:\workspace\packages\id.TOMBSTONE"
        )));
        assert!(path_uses_tombstone_namespace(Path::new(
            r"C:\workspace\packages\id.tombstone. "
        )));
        assert!(path_uses_tombstone_namespace(Path::new(
            r"C:\workspace\packages\id.tombstone:stream"
        )));
    }

    #[cfg(windows)]
    #[test]
    fn quarantine_write_through_never_replaces_a_tombstone() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let partial = temporary.path().join("source.part");
        let tombstone = temporary.path().join("cleanup.tombstone");
        fs::write(&partial, b"live partial").expect("partial");
        fs::write(&tombstone, b"existing tombstone").expect("tombstone");

        let error = quarantine_write_through(&partial, &tombstone).expect_err("must not replace");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read(&partial).expect("partial remains"),
            b"live partial"
        );
        assert_eq!(
            fs::read(&tombstone).expect("tombstone remains"),
            b"existing tombstone"
        );
    }

    #[cfg(windows)]
    #[test]
    fn quarantine_rejects_a_different_parent_before_the_os_move() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let live_namespace = temporary.path().join("packages");
        let quarantine_namespace = temporary.path().join("other");
        fs::create_dir_all(&live_namespace).expect("live namespace");
        fs::create_dir_all(&quarantine_namespace).expect("quarantine namespace");
        let partial = live_namespace.join("source.part");
        let tombstone = quarantine_namespace.join("cleanup.tombstone");
        fs::write(&partial, b"live partial").expect("partial");

        let error = quarantine_write_through(&partial, &tombstone)
            .expect_err("different parent must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::CrossesDevices);
        assert_eq!(
            fs::read(&partial).expect("partial remains"),
            b"live partial"
        );
        assert!(!tombstone.exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_same_length_path_replacement_fails_identity_verification() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("evidence.bin");
        let displaced = temporary.path().join("displaced.bin");
        fs::write(&path, b"original").expect("original file");
        let retained = fs::File::open(&path).expect("retained handle");
        fs::rename(&path, &displaced).expect("displace original name");
        fs::write(&path, b"replaced").expect("same-length replacement");

        assert!(!path_names_open_file(&path, &retained).expect("identity check"));
        assert_eq!(
            fs::read(&displaced).expect("displaced original"),
            b"original"
        );
        assert_eq!(fs::read(&path).expect("replacement"), b"replaced");
    }
}

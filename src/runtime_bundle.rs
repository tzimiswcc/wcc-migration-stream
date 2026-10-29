//! Canonical, checksummed assisted-local-runtime bundle authority.
//!
//! The manifest is an internal-consistency contract, not a release signature.
//! It binds every shipped runtime member, rejects undeclared material, and
//! gives the launcher one exact local runtime graph to verify before use.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::Command,
};

#[cfg(windows)]
use std::{
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zip::{CompressionMethod, DateTime, System, ZipArchive, ZipWriter, write::SimpleFileOptions};

use crate::{
    KernelError, Result,
    canonical::{CANONICAL_JSON_VERSION, canonical_json_bytes, parse_unique_json, sha256_hex},
};

pub const BUNDLE_FORMAT_VERSION: &str = "migration-stream/assisted-local-runtime-bundle/v2";
pub const BUNDLE_MANIFEST_FILENAME: &str = "bundle-manifest.json";
pub const BUNDLE_OPERATOR_GUIDE_FILENAME: &str = "OPERATOR-GUIDE.md";
pub const BUNDLE_TARGET_MATRIX_VERSION: &str =
    "migration-stream/assisted-local-runtime-target-matrix/v1";
pub const SUPPORTED_RUNTIME_OS: &str = "macos";
pub const SUPPORTED_RUNTIME_ARCHITECTURE: &str = "arm64";
pub const SUPPORTED_RUNTIME_TARGET_TRIPLE: &str = "aarch64-apple-darwin";
pub const SUPPORTED_RUNTIME_MACOS_MAJOR: &str = "26";
/// Exact provisional native qualification host; this is not a support/release
/// promotion or an allowance for arbitrary macOS 27/future versions.
pub const PROVISIONAL_RUNTIME_MACOS_VERSION: &str = "27.0.1";
pub const BUNDLED_BINARY_MINIMUM_MACOS_VERSION: &str = "11.0";
pub const COMPILED_PRODUCT_COMMIT: Option<&str> = option_env!("MIGRATION_STREAM_PRODUCT_COMMIT");
pub const COMPILED_SOURCE_SNAPSHOT_SHA256: Option<&str> =
    option_env!("MIGRATION_STREAM_SOURCE_SNAPSHOT_SHA256");
pub const COMPILED_SOURCE_STATE: Option<&str> = option_env!("MIGRATION_STREAM_SOURCE_STATE");
pub const COMPILED_FRONTEND_ASSETS_SHA256: Option<&str> =
    option_env!("MIGRATION_STREAM_FRONTEND_ASSETS_SHA256");

const ZERO_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const MAXIMUM_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAXIMUM_MEMBER_COUNT: usize = 1024;
const MAXIMUM_MEMBER_BYTES: u64 = 128 * 1024 * 1024;
const MAXIMUM_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;
const MAXIMUM_FRONTEND_MEMBER_COUNT: usize = 256;
const MAXIMUM_FRONTEND_MEMBER_BYTES: u64 = 8 * 1024 * 1024;
const MAXIMUM_FRONTEND_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const MAXIMUM_FRONTEND_GRAPH_ENTRY_COUNT: usize =
    MAXIMUM_FRONTEND_MEMBER_COUNT * (MAXIMUM_PATH_BYTES / 2 + 2);
const MAXIMUM_PATH_BYTES: usize = 512;
#[cfg(any(not(windows), test))]
const MAXIMUM_ROLLBACK_ENTRY_COUNT: usize = MAXIMUM_MEMBER_COUNT * (MAXIMUM_PATH_BYTES / 2 + 2) + 1;
const MAXIMUM_PRODUCT_VERSION_BYTES: usize = 64;
const MAXIMUM_PRODUCT_COMMIT_BYTES: usize = 64;
const MAXIMUM_SOURCE_FILE_COUNT: usize = 100_000;
const MAXIMUM_SOURCE_INVENTORY_BYTES: usize = 16 * 1024 * 1024;
const MAXIMUM_SOURCE_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAXIMUM_SOURCE_TOTAL_BYTES: u64 = 2 * 1024 * 1024 * 1024;
#[cfg(windows)]
const WINDOWS_BUNDLE_DIRECTORY_PUBLISH_RETRY_WINDOW: Duration = Duration::from_secs(5);
#[cfg(windows)]
const WINDOWS_BUNDLE_DIRECTORY_PUBLISH_RETRY_DELAY: Duration = Duration::from_millis(100);
#[cfg(windows)]
const WINDOWS_BUNDLE_DIRECTORY_PUBLISH_MAXIMUM_ATTEMPTS: usize = 51;

const POSIX_LAUNCHER_PATH: &str = "migration-stream-local";
const WINDOWS_LAUNCHER_PATH: &str = "migration-stream-local.exe";
const POSIX_RUNTIME_CONTROLLER_PATH: &str = "libexec/migration-stream-local-runtime";
const WINDOWS_RUNTIME_CONTROLLER_PATH: &str = "libexec/migration-stream-local-runtime.exe";
const POSIX_CLI_PATH: &str = "bin/migration-stream";
const WINDOWS_CLI_PATH: &str = "bin/migration-stream.exe";
const POSIX_WORKBENCH_PATH: &str = "bin/migration-stream-workbench";
const WINDOWS_WORKBENCH_PATH: &str = "bin/migration-stream-workbench.exe";
// Legacy aliases keep the existing macOS fixture tests explicit. Production
// assembly and replay always derive paths from the manifest target.
#[cfg(test)]
const LAUNCHER_PATH: &str = POSIX_LAUNCHER_PATH;
#[cfg(test)]
const RUNTIME_CONTROLLER_PATH: &str = POSIX_RUNTIME_CONTROLLER_PATH;
#[cfg(test)]
const CLI_PATH: &str = POSIX_CLI_PATH;
#[cfg(test)]
const WORKBENCH_PATH: &str = POSIX_WORKBENCH_PATH;
const INTAKE_POLICY_PATH: &str = "config/engagement-intake-policy.json";
const PROFILING_POLICY_PATH: &str = "config/engagement-profiling-policy.json";
const VALIDATION_POLICY_PATH: &str = "config/engagement-validation-policy.json";
const PROFILE_SCHEMA_PATH: &str = "schemas/migration-object-profile.schema.json";
const MAPPING_SCHEMA_PATH: &str = "schemas/source-mapping-version.schema.json";
const BUNDLE_SCHEMA_PATH: &str = "schemas/assisted-runtime-bundle.schema.json";
const FRONTEND_INDEX_PATH: &str = "web/index.html";
const CONTROLLED_PACK_PATHS: [&str; 4] = [
    "object-packs/synthetic.customer.readiness/1.0.0/manifest.json",
    "object-packs/synthetic.customer.readiness/1.0.0/synthetic-profile.reviewed.json",
    "object-packs/synthetic.sales-document.readiness/1.0.0/manifest.json",
    "object-packs/synthetic.sales-document.readiness/1.0.0/synthetic-profile.reviewed.json",
];
const POSIX_PLATFORM_LAUNCHER: &str = r#"#!/bin/sh
set -eu
detected_os=$(/usr/bin/uname -s 2>/dev/null || printf 'unknown')
detected_arch=$(/usr/bin/uname -m 2>/dev/null || printf 'unknown')
case "$detected_os/$detected_arch" in
  Darwin/arm64|Linux/x86_64|Linux/aarch64) ;;
  *)
  printf 'Migration Stream assisted local runtime requires the exact bundle platform; detected %s/%s.\n' "$detected_os" "$detected_arch" >&2
  exit 1
  ;;
esac
case "$0" in
  */*) launcher_directory=${0%/*} ;;
  *) launcher_directory=. ;;
esac
bundle_root=$(CDPATH= cd -- "$launcher_directory" && pwd -P)
exec /usr/bin/env -i "$bundle_root/libexec/migration-stream-local-runtime" "$@"
"#;

#[cfg(test)]
const PLATFORM_LAUNCHER: &str = POSIX_PLATFORM_LAUNCHER;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AssistedRuntimeBundleManifest {
    pub schema_version: String,
    pub canonical_json_version: String,
    pub manifest_sha256: String,
    pub product: BundleProduct,
    pub target: BundleTarget,
    pub entrypoints: BundleEntrypoints,
    pub members: Vec<BundleMember>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleProduct {
    pub name: String,
    pub version: String,
    pub commit: String,
    pub source_state: BundleSourceState,
    pub source_snapshot_sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BundleSourceState {
    Clean,
    ExplicitUncommittedSnapshot,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleTarget {
    pub os: String,
    pub architecture: String,
    pub target_triple: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_os_version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleEntrypoints {
    pub launcher: String,
    pub runtime_controller: String,
    pub integrity_cli: String,
    pub workbench: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleMember {
    pub path: String,
    pub member_type: BundleMemberType,
    pub role: BundleMemberRole,
    pub mode: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BundleMemberType {
    RegularFile,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BundleMemberRole {
    PlatformLauncher,
    RuntimeController,
    IntegrityCli,
    Workbench,
    FrontendAsset,
    Policy,
    ProfileSchema,
    MappingSchema,
    BundleSchema,
    ControlledPack,
    OperatorGuide,
}

impl BundleMemberRole {
    const fn expected_mode(self) -> &'static str {
        match self {
            Self::PlatformLauncher
            | Self::RuntimeController
            | Self::IntegrityCli
            | Self::Workbench => "0555",
            Self::FrontendAsset
            | Self::Policy
            | Self::ProfileSchema
            | Self::MappingSchema
            | Self::BundleSchema
            | Self::ControlledPack
            | Self::OperatorGuide => "0444",
        }
    }

    const fn maximum_bytes(self) -> u64 {
        match self {
            Self::PlatformLauncher
            | Self::RuntimeController
            | Self::IntegrityCli
            | Self::Workbench => MAXIMUM_MEMBER_BYTES,
            Self::FrontendAsset => MAXIMUM_FRONTEND_MEMBER_BYTES,
            Self::Policy
            | Self::ProfileSchema
            | Self::MappingSchema
            | Self::BundleSchema
            | Self::ControlledPack
            | Self::OperatorGuide => 8 * 1024 * 1024,
        }
    }
}

/// The complete portable-bundle target matrix. A matrix entry says which
/// artifact may be assembled and verified; it does not claim that every entry
/// has passed native-device acceptance or is publicly released.
#[must_use]
pub fn supported_bundle_targets() -> Vec<BundleTarget> {
    vec![
        BundleTarget {
            os: "macos".to_owned(),
            architecture: "arm64".to_owned(),
            target_triple: "aarch64-apple-darwin".to_owned(),
            minimum_os_version: Some(BUNDLED_BINARY_MINIMUM_MACOS_VERSION.to_owned()),
        },
        BundleTarget {
            os: "windows".to_owned(),
            architecture: "x64".to_owned(),
            target_triple: "x86_64-pc-windows-msvc".to_owned(),
            minimum_os_version: None,
        },
        BundleTarget {
            os: "windows".to_owned(),
            architecture: "arm64".to_owned(),
            target_triple: "aarch64-pc-windows-msvc".to_owned(),
            minimum_os_version: None,
        },
        BundleTarget {
            os: "linux".to_owned(),
            architecture: "x64".to_owned(),
            target_triple: "x86_64-unknown-linux-gnu".to_owned(),
            minimum_os_version: None,
        },
        BundleTarget {
            os: "linux".to_owned(),
            architecture: "arm64".to_owned(),
            target_triple: "aarch64-unknown-linux-gnu".to_owned(),
            minimum_os_version: None,
        },
    ]
}

fn target_for(os: &str, architecture: &str) -> Result<BundleTarget> {
    supported_bundle_targets()
        .into_iter()
        .find(|target| target.os == os && target.architecture == architecture)
        .ok_or_else(|| {
            KernelError::BundlePlatform(format!(
                "unsupported assisted-runtime target {os}-{architecture}"
            ))
        })
}

fn launcher_path_for(target: &BundleTarget) -> &'static str {
    if target.os == "windows" {
        WINDOWS_LAUNCHER_PATH
    } else {
        POSIX_LAUNCHER_PATH
    }
}

fn runtime_controller_path_for(target: &BundleTarget) -> &'static str {
    if target.os == "windows" {
        WINDOWS_RUNTIME_CONTROLLER_PATH
    } else {
        POSIX_RUNTIME_CONTROLLER_PATH
    }
}

fn cli_path_for(target: &BundleTarget) -> &'static str {
    if target.os == "windows" {
        WINDOWS_CLI_PATH
    } else {
        POSIX_CLI_PATH
    }
}

fn workbench_path_for(target: &BundleTarget) -> &'static str {
    if target.os == "windows" {
        WINDOWS_WORKBENCH_PATH
    } else {
        POSIX_WORKBENCH_PATH
    }
}

fn platform_launcher_for(target: &BundleTarget) -> Result<&'static str> {
    if target.os == "windows" {
        return Err(bundle_invalid(
            "Windows command launcher must be the exact native controller executable",
        ));
    }
    Ok(POSIX_PLATFORM_LAUNCHER)
}

fn target_bundle_assembly_enabled(target: &BundleTarget) -> bool {
    // An exact Windows target may be assembled only as a native acceptance
    // candidate. The feature does not alter matrix or release status.
    // This is deliberately separate from the machine-readable acceptance and
    // release status, which remain blocked until native evidence is accepted.
    target.os != "windows"
        || (cfg!(feature = "windows-native-acceptance-candidate")
            && matches!(
                (target.architecture.as_str(), target.target_triple.as_str()),
                ("x64", "x86_64-pc-windows-msvc") | ("arm64", "aarch64-pc-windows-msvc")
            ))
}

fn binary_name_for(target: &BundleTarget, base: &str) -> String {
    if target.os == "windows" {
        format!("{base}.exe")
    } else {
        base.to_owned()
    }
}

#[derive(Clone, Debug)]
pub struct BundleAssemblyRequest {
    pub repository_root: PathBuf,
    /// One concrete Git executable supplied by the build driver.  Bundle
    /// provenance must never depend on ambient PATH resolution.
    pub git_executable: PathBuf,
    pub frontend_assets_root: PathBuf,
    /// Private, freshly copied release executables. This intentionally stays
    /// outside the repository: Cargo may legitimately expose an output through
    /// a hard-link alias, while a bundle member must be a single-link inode.
    pub release_binaries_root: PathBuf,
    pub output_directory: PathBuf,
    pub product_commit: String,
    pub source_state: BundleSourceState,
    pub source_snapshot_sha256: String,
    pub frontend_assets_sha256: String,
    pub create_zip_archive: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleAssemblyResult {
    pub bundle_directory: PathBuf,
    pub manifest_sha256: String,
    pub member_count: usize,
    pub total_member_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_checksum_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleVerification {
    pub bundle_root: PathBuf,
    pub manifest_sha256: String,
    pub product_commit: String,
    pub product_version: String,
    pub source_state: BundleSourceState,
    pub source_snapshot_sha256: String,
    pub target: BundleTarget,
    pub launcher_sha256: String,
    pub runtime_controller_sha256: String,
    pub integrity_cli_sha256: String,
    pub workbench_sha256: String,
    pub member_count: usize,
    pub total_member_bytes: u64,
}

/// Explicit inputs for a create-only, local Windows engineering layout.
/// The expected launcher digest binds bytes, not publisher or compiler provenance.
#[derive(Debug)]
pub struct WindowsEngineeringLayoutRequest {
    pub launcher: PathBuf,
    pub expected_launcher_sha256: String,
    pub runtime: PathBuf,
    pub output_directory: PathBuf,
    pub engineering_policy: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowsEngineeringLayoutResult {
    pub output_directory: PathBuf,
    /// Outer document launcher; distinct from the runtime's platform launcher.
    pub document_launcher_sha256: String,
    pub runtime: BundleVerification,
}

/// Assemble an already-built launcher beside an intact runtime without running either.
///
/// # Errors
/// Refuses non-Windows hosts, invalid policy/paths/digests/PE targets, changed or
/// oversized inputs, runtime verification failure, and publication collisions.
/// Windows cleanup preserves an unpublished partial when exact recursive removal
/// cannot be established. Visible output is never removed after publication.
pub fn assemble_windows_engineering_layout(
    request: &WindowsEngineeringLayoutRequest,
) -> Result<WindowsEngineeringLayoutResult> {
    if std::env::consts::OS != "windows" {
        return Err(bundle_invalid(
            "Windows engineering layout requires a native Windows host",
        ));
    }
    assemble_windows_engineering_layout_with(request, |path| verify_bundle_root(path))
}

const ENGINEERING_LAUNCHER: &str = "migration-stream-document-open.exe";
const ENGINEERING_GRAPH_ENTRIES: usize = 4096;
const ENGINEERING_GRAPH_DEPTH: usize = 64;

struct EngineeringNode {
    relative: PathBuf,
    identity: OwnedPathIdentity,
    bytes: Option<Vec<u8>>,
    mode: u32,
}

// Capture before calling the verifier, so even a hostile unverified graph has
// finite entry/depth/byte bounds. read_dir streams; admission precedes descent.
fn engineering_snapshot(root: &Path) -> Result<Vec<EngineeringNode>> {
    let mut nodes = Vec::new();
    let mut bytes = 0_u64;
    engineering_snapshot_directory(root, root, &mut nodes, &mut bytes, 0)?;
    nodes.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(nodes)
}

fn engineering_snapshot_directory(
    root: &Path,
    directory: &Path,
    nodes: &mut Vec<EngineeringNode>,
    total: &mut u64,
    depth: usize,
) -> Result<()> {
    if depth > ENGINEERING_GRAPH_DEPTH || nodes.len() >= ENGINEERING_GRAPH_ENTRIES {
        return Err(bundle_invalid("engineering layout graph exceeds its bound"));
    }
    let identity = capture_owned_directory(directory)?;
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| bundle_invalid("engineering source escaped root"))?
        .to_path_buf();
    nodes.push(EngineeringNode {
        relative,
        mode: engineering_mode(&identity.metadata),
        identity,
        bytes: None,
    });
    for entry in fs::read_dir(directory).map_err(|e| KernelError::io(directory, e))? {
        let entry = entry.map_err(|e| KernelError::io(directory, e))?;
        if nodes.len() >= ENGINEERING_GRAPH_ENTRIES {
            return Err(bundle_invalid("engineering layout graph exceeds its bound"));
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| bundle_invalid("engineering source escaped root"))?
            .to_path_buf();
        validate_member_path(&path_to_manifest_string(&relative)?)?;
        let metadata = fs::symlink_metadata(&path).map_err(|e| KernelError::io(&path, e))?;
        if metadata.file_type().is_symlink() {
            return Err(bundle_invalid("engineering source contains a link"));
        }
        if metadata.is_dir() {
            engineering_snapshot_directory(root, &path, nodes, total, depth + 1)?;
        } else {
            let (file, metadata) = open_retained_regular_file(&path)?;
            let identity = capture_open_file_identity(&path, &file)?;
            let bytes = read_regular_file_bounded(&path, MAXIMUM_MEMBER_BYTES)?;
            *total = total
                .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                .filter(|n| *n <= MAXIMUM_BUNDLE_BYTES + MAXIMUM_MANIFEST_BYTES)
                .ok_or_else(|| bundle_invalid("engineering runtime exceeds total byte bound"))?;
            if !same_owned_file_identity(&path, &file, &metadata, &identity)? {
                return Err(bundle_invalid("engineering source changed during capture"));
            }
            nodes.push(EngineeringNode {
                relative,
                identity,
                mode: engineering_mode(&metadata),
                bytes: Some(bytes),
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn engineering_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt as _;
    metadata.mode() & 0o777
}

#[cfg(not(unix))]
fn engineering_mode(_metadata: &fs::Metadata) -> u32 {
    0o444
}

fn engineering_same_content(a: &[EngineeringNode], b: &[EngineeringNode]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a.relative == b.relative && a.bytes == b.bytes && a.mode == b.mode)
}

fn replay_engineering_snapshot(root: &Path, expected: &[EngineeringNode]) -> Result<()> {
    let current = engineering_snapshot(root)?;
    if current.len() != expected.len() {
        return Err(bundle_invalid("engineering source graph changed"));
    }
    for (a, b) in expected.iter().zip(current.iter()) {
        let path = root.join(&a.relative);
        if a.relative != b.relative || a.bytes != b.bytes || a.mode != b.mode {
            return Err(bundle_invalid("engineering source bytes or graph changed"));
        }
        if a.bytes.is_some() {
            let (file, metadata) = open_retained_regular_file(&path)?;
            if !same_owned_file_identity(&path, &file, &metadata, &a.identity)? {
                return Err(bundle_invalid("engineering source file replaced"));
            }
        } else {
            verify_owned_bundle_directory(
                &path,
                &a.identity,
                "engineering source directory replaced",
            )?;
        }
    }
    Ok(())
}

fn engineering_same_verification(a: &BundleVerification, b: &BundleVerification) -> Result<()> {
    let mut a = serde_json::to_value(a)?;
    let mut b = serde_json::to_value(b)?;
    a.as_object_mut()
        .expect("verification object")
        .remove("bundleRoot");
    b.as_object_mut()
        .expect("verification object")
        .remove("bundleRoot");
    if a != b {
        return Err(bundle_invalid(
            "engineering runtime verification identity changed",
        ));
    }
    Ok(())
}

fn engineering_absolute(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(bundle_invalid(
            "engineering input must be one explicit absolute path",
        ));
    }
    Ok(())
}

struct EngineeringInputs {
    runtime_identity: OwnedPathIdentity,
    runtime: PathBuf,
    launcher_file: File,
    launcher_metadata: fs::Metadata,
    launcher_identity: OwnedPathIdentity,
    launcher: PathBuf,
    parent_identity: OwnedPathIdentity,
    parent: PathBuf,
    output: PathBuf,
    name: String,
}

// Capture the selected spelling before canonicalization: Windows capabilities
// reject reparse ancestors and aliases rather than silently following them.
fn engineering_inputs(request: &WindowsEngineeringLayoutRequest) -> Result<EngineeringInputs> {
    let runtime_identity = capture_owned_directory(&request.runtime)?;
    let runtime = require_real_directory(&request.runtime, "engineering runtime")?;
    let (launcher_file, launcher_metadata) = open_retained_regular_file(&request.launcher)?;
    let launcher_identity = capture_open_file_identity(&request.launcher, &launcher_file)?;
    let launcher = request
        .launcher
        .canonicalize()
        .map_err(|e| KernelError::io(&request.launcher, e))?;
    let parent = request
        .output_directory
        .parent()
        .ok_or_else(|| bundle_invalid("output has no parent"))?;
    let parent_identity = capture_owned_directory(parent)?;
    let parent = require_real_directory(parent, "engineering output parent")?;
    let name = request
        .output_directory
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| safe_component(s))
        .ok_or_else(|| bundle_invalid("unsafe engineering output name"))?;
    let output = parent.join(name);
    if path_lexically_exists(&output)?
        || launcher.starts_with(&runtime)
        || output.starts_with(&runtime)
        || runtime.starts_with(&output)
        || launcher.starts_with(&output)
    {
        return Err(bundle_invalid(
            "engineering output exists or inputs overlap",
        ));
    }
    Ok(EngineeringInputs {
        runtime_identity,
        runtime,
        launcher_file,
        launcher_metadata,
        launcher_identity,
        launcher,
        parent_identity,
        parent,
        output,
        name: name.to_owned(),
    })
}

fn copy_engineering_runtime(stage: &Path, snapshot: &[EngineeringNode]) -> Result<PathBuf> {
    let staged_runtime = stage.join("Runtime");
    for node in snapshot {
        let path = staged_runtime.join(&node.relative);
        if let Some(bytes) = &node.bytes {
            write_new_file(&path, bytes, node.mode)?;
        } else {
            create_owned_directory(&path)?;
        }
    }
    // Restore source directory modes only after their children are complete.
    for node in snapshot.iter().rev().filter(|n| n.bytes.is_none()) {
        set_mode(&staged_runtime.join(&node.relative), node.mode)?;
    }
    Ok(staged_runtime)
}

// The outer launcher proof must bind the exact capture subsequently hashed and
// copied. A separate named-file PE read can prove a different version of bytes.
fn verify_engineering_launcher_bytes(bytes: &[u8], target: &BundleTarget) -> Result<()> {
    let machine = match (target.os.as_str(), target.architecture.as_str()) {
        ("windows", "x64") => 0x8664_u16,
        ("windows", "arm64") => 0xaa64_u16,
        _ => {
            return Err(bundle_invalid(
                "engineering launcher requires a Windows PE64 target",
            ));
        }
    };
    if bytes.len() < 256 || u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAXIMUM_MEMBER_BYTES {
        return Err(bundle_invalid(
            "captured engineering launcher exceeds PE64 size bounds",
        ));
    }
    let offset = usize::try_from(u32::from_le_bytes([
        bytes[60], bytes[61], bytes[62], bytes[63],
    ]))
    .map_err(|_| bundle_invalid("captured PE header offset exceeds host bounds"))?;
    if offset > 1024 * 1024 || offset.saturating_add(26) > bytes.len() {
        return Err(bundle_invalid(
            "captured PE header offset exceeds its bound",
        ));
    }
    let header = &bytes[offset..offset + 26];
    if bytes[..2] != *b"MZ"
        || header[..4] != *b"PE\0\0"
        || u16::from_le_bytes([header[4], header[5]]) != machine
        || u16::from_le_bytes([header[24], header[25]]) != 0x20b
    {
        return Err(bundle_invalid(
            "captured engineering launcher is not the expected PE64 target",
        ));
    }
    Ok(())
}

fn verify_engineering_outer(root: &Path, launcher_bytes: &[u8]) -> Result<()> {
    let names = fs::read_dir(root)
        .map_err(|e| KernelError::io(root, e))?
        .take(3)
        .map(|entry| {
            entry
                .map(|e| e.file_name())
                .map_err(|e| KernelError::io(root, e))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if names
        != BTreeSet::from([
            std::ffi::OsString::from(ENGINEERING_LAUNCHER),
            std::ffi::OsString::from("Runtime"),
        ])
        || read_regular_file_bounded(&root.join(ENGINEERING_LAUNCHER), MAXIMUM_MEMBER_BYTES)?
            != launcher_bytes
    {
        return Err(bundle_invalid("engineering outer layout differs"));
    }
    Ok(())
}

fn publish_engineering_layout<F>(
    stage: &Path,
    output: &Path,
    stage_identity: &OwnedPathIdentity,
    source: &BundleVerification,
    launcher_bytes: &[u8],
    verify: &mut F,
) -> Result<BundleVerification>
where
    F: FnMut(&Path) -> Result<BundleVerification>,
{
    if let Err(error) = verify_engineering_outer(stage, launcher_bytes) {
        return Err(rollback_assembly_outputs(
            error,
            Some((stage, stage_identity)),
            None,
            None,
        ));
    }
    if let Err(failure) = publish_bundle_directory_noreplace(stage, output, stage_identity) {
        return Err(if failure.published {
            failure.error
        } else {
            rollback_assembly_outputs(failure.error, Some((stage, stage_identity)), None, None)
        });
    }
    // A post-move failure preserves the visible result for explicit inspection.
    sync_directory(
        output
            .parent()
            .ok_or_else(|| bundle_invalid("output has no parent"))?,
    )?;
    let final_runtime = verify(&output.join("Runtime"))?;
    engineering_same_verification(source, &final_runtime)?;
    verify_engineering_outer(output, launcher_bytes)?;
    Ok(final_runtime)
}

fn validate_engineering_request(request: &WindowsEngineeringLayoutRequest) -> Result<()> {
    if request.engineering_policy != "local-unsigned-engineering" {
        return Err(bundle_invalid(
            "explicit local-unsigned-engineering policy required",
        ));
    }
    validate_sha256(
        &request.expected_launcher_sha256,
        "document launcher SHA-256",
    )?;
    for path in [
        &request.launcher,
        &request.runtime,
        &request.output_directory,
    ] {
        engineering_absolute(path)?;
    }
    Ok(())
}

fn assemble_windows_engineering_layout_with<F>(
    request: &WindowsEngineeringLayoutRequest,
    mut verify: F,
) -> Result<WindowsEngineeringLayoutResult>
where
    F: FnMut(&Path) -> Result<BundleVerification>,
{
    validate_engineering_request(request)?;
    let EngineeringInputs {
        runtime_identity,
        runtime,
        launcher_file,
        launcher_metadata,
        launcher_identity,
        launcher,
        parent_identity,
        parent,
        output,
        name,
    } = engineering_inputs(request)?;
    let snapshot = engineering_snapshot(&runtime)?;
    let source = verify(&runtime)?;
    if source.target.os != "windows"
        || source.target != target_for("windows", &source.target.architecture)?
    {
        return Err(bundle_invalid(
            "engineering runtime is not an exact Windows target",
        ));
    }
    let launcher_bytes = read_regular_file_bounded(&launcher, MAXIMUM_MEMBER_BYTES)?;
    verify_engineering_launcher_bytes(&launcher_bytes, &source.target)?;
    if sha256_hex(&launcher_bytes) != request.expected_launcher_sha256 {
        return Err(bundle_invalid("document launcher digest differs"));
    }
    let stage = parent.join(format!(".{name}.part-{}", Uuid::new_v4()));
    let stage_identity = create_owned_directory(&stage)?;
    let prepared = (|| {
        write_new_file(
            &stage.join(ENGINEERING_LAUNCHER),
            &launcher_bytes,
            engineering_mode(&launcher_metadata),
        )?;
        let staged_runtime = copy_engineering_runtime(&stage, &snapshot)?;
        let staged = verify(&staged_runtime)?;
        let copied = engineering_snapshot(&staged_runtime)?;
        if !engineering_same_content(&snapshot, &copied) {
            return Err(bundle_invalid("staged runtime bytes or graph differ"));
        }
        engineering_same_verification(&source, &staged)?;
        replay_engineering_snapshot(&runtime, &snapshot)?;
        engineering_same_verification(&source, &verify(&runtime)?)?;
        verify_owned_bundle_directory(
            &request.runtime,
            &runtime_identity,
            "runtime root replaced",
        )?;
        verify_named_open_identity(&request.launcher, &launcher_file, &launcher_metadata)?;
        if !same_owned_file_identity(
            &request.launcher,
            &launcher_file,
            &launcher_metadata,
            &launcher_identity,
        )? || read_regular_file_bounded(&launcher, MAXIMUM_MEMBER_BYTES)? != launcher_bytes
        {
            return Err(bundle_invalid("document launcher changed"));
        }
        verify_owned_bundle_directory(
            &parent,
            &parent_identity,
            "engineering output parent replaced",
        )?;
        Ok(())
    })();
    if let Err(error) = prepared {
        return Err(rollback_assembly_outputs(
            error,
            Some((&stage, &stage_identity)),
            None,
            None,
        ));
    }
    let final_runtime = publish_engineering_layout(
        &stage,
        &output,
        &stage_identity,
        &source,
        &launcher_bytes,
        &mut verify,
    )?;
    Ok(WindowsEngineeringLayoutResult {
        output_directory: output,
        document_launcher_sha256: request.expected_launcher_sha256.clone(),
        runtime: final_runtime,
    })
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundleBuildIdentity {
    pub schema_version: String,
    pub product_version: String,
    pub product_commit: String,
    pub source_state: BundleSourceState,
    pub source_snapshot_sha256: String,
    pub frontend_assets_sha256: String,
    pub target: BundleTarget,
}

#[derive(Clone, Debug)]
struct CopySpecification {
    source: PathBuf,
    destination: &'static str,
    role: BundleMemberRole,
}

#[derive(Default)]
struct TreeCopyProgress {
    entries: usize,
    files: usize,
    bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceSnapshotManifest {
    schema_version: &'static str,
    files: Vec<SourceSnapshotMember>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceSnapshotMember {
    path: String,
    executable: bool,
    bytes: u64,
    sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FrontendAssetSnapshotManifest {
    schema_version: &'static str,
    files: Vec<FrontendAssetSnapshotMember>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FrontendAssetSnapshotMember {
    path: String,
    bytes: u64,
    sha256: String,
}

struct PartialFileGuard {
    path: PathBuf,
    identity: OwnedPathIdentity,
    published: bool,
}

#[derive(Clone, Debug)]
struct OwnedPathIdentity {
    metadata: fs::Metadata,
    // Windows metadata exposes no stable object identity. Keep the complete
    // FileId captured from the same opened handle as the metadata so a
    // same-length replacement is never accepted as this output.
    #[cfg(windows)]
    native_identity: migration_stream_windows_native::PathIdentity,
}

#[derive(Debug)]
struct PublishedArchive {
    sha256: String,
    identity: OwnedPathIdentity,
}

impl Drop for PartialFileGuard {
    fn drop(&mut self) {
        if !self.published {
            let _ = remove_exact_owned_file(&self.path, &self.identity);
        }
    }
}

fn capture_owned_directory(path: &Path) -> Result<OwnedPathIdentity> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(bundle_invalid("published output is not one real directory"));
    }
    #[cfg(windows)]
    let native_identity = migration_stream_windows_native::RealDirectory::capture(path)
        .map_err(|error| KernelError::io(path, error))?
        .identity();
    Ok(OwnedPathIdentity {
        metadata,
        #[cfg(windows)]
        native_identity,
    })
}

/// Capture an output identity from its already-open file handle. On Windows,
/// `Metadata` is deliberately insufficient: the retained `FileId` is the
/// authority used for every later replay and rollback check.
fn capture_open_file_identity(path: &Path, file: &File) -> Result<OwnedPathIdentity> {
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    #[cfg(windows)]
    let native_identity = migration_stream_windows_native::file_identity_from_open_file(file)
        .map_err(|error| KernelError::io(path, error))?;
    Ok(OwnedPathIdentity {
        metadata,
        #[cfg(windows)]
        native_identity,
    })
}

#[cfg_attr(not(windows), allow(clippy::unnecessary_wraps))]
fn same_owned_file_identity(
    path: &Path,
    file: &File,
    metadata: &fs::Metadata,
    expected: &OwnedPathIdentity,
) -> Result<bool> {
    #[cfg(windows)]
    let matches = {
        let _ = metadata;
        let named_identity = migration_stream_windows_native::RealFile::capture(path)
            .map_err(|error| KernelError::io(path, error))?
            .identity();
        let opened_identity = migration_stream_windows_native::file_identity_from_open_file(file)
            .map_err(|error| KernelError::io(path, error))?;
        named_identity == opened_identity && opened_identity == expected.native_identity
    };
    #[cfg(not(windows))]
    let matches = {
        let opened = file
            .metadata()
            .map_err(|error| KernelError::io(path, error))?;
        same_file_identity(metadata, &opened) && same_file_identity(&opened, &expected.metadata)
    };
    Ok(matches)
}

#[cfg_attr(not(windows), allow(clippy::unnecessary_wraps))]
fn same_owned_directory_identity(
    path: &Path,
    metadata: &fs::Metadata,
    expected: &OwnedPathIdentity,
) -> Result<bool> {
    #[cfg(windows)]
    let matches = {
        let _ = metadata;
        let opened = migration_stream_windows_native::RealDirectory::capture(path)
            .map_err(|error| KernelError::io(path, error))?;
        opened.identity() == expected.native_identity
    };
    #[cfg(not(windows))]
    let matches = {
        let _ = path;
        same_file_identity(metadata, &expected.metadata)
    };
    Ok(matches)
}

/// Return the one explicitly supported assisted-runtime target.
///
/// # Errors
///
/// Returns `BUNDLE_PLATFORM_MISMATCH` on every unproven host.
pub fn supported_host_target() -> Result<BundleTarget> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        let detected_version = detected_macos_version()?;
        qualify_macos_host(&detected_version, std::env::consts::ARCH)?;
        target_for(SUPPORTED_RUNTIME_OS, SUPPORTED_RUNTIME_ARCHITECTURE)
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        target_for("linux", "x64")
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        target_for("linux", "arm64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        target_for("windows", "x64")
    } else if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        target_for("windows", "arm64")
    } else {
        Err(KernelError::BundlePlatform(format!(
            "no assisted-runtime matrix target matches detected {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MacosHostQualification {
    Existing26,
    Provisional27_0_1,
}

fn qualify_macos_host(version: &str, architecture: &str) -> Result<MacosHostQualification> {
    match (architecture, version) {
        ("aarch64", value) if value.split('.').next() == Some(SUPPORTED_RUNTIME_MACOS_MAJOR) => {
            Ok(MacosHostQualification::Existing26)
        }
        ("aarch64", PROVISIONAL_RUNTIME_MACOS_VERSION) => {
            Ok(MacosHostQualification::Provisional27_0_1)
        }
        _ => Err(KernelError::BundlePlatform(format!(
            "qualified macOS 26 arm64 or exact provisional macOS {PROVISIONAL_RUNTIME_MACOS_VERSION} arm64 candidate required; detected macOS {version} {architecture}"
        ))),
    }
}

fn detected_macos_version() -> Result<String> {
    if !cfg!(target_os = "macos") {
        return Err(KernelError::BundlePlatform(
            "macOS version detection is unavailable on this host".to_owned(),
        ));
    }
    let output = Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .map_err(|error| KernelError::io("/usr/bin/sw_vers", error))?;
    if !output.status.success() || output.stdout.len() > 64 || !output.stderr.is_empty() {
        return Err(KernelError::BundlePlatform(
            "macOS product version could not be verified".to_owned(),
        ));
    }
    let version = std::str::from_utf8(&output.stdout)
        .map_err(|_| KernelError::BundlePlatform("macOS product version is not UTF-8".to_owned()))?
        .trim();
    if version.is_empty()
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return Err(KernelError::BundlePlatform(
            "macOS product version is invalid".to_owned(),
        ));
    }
    Ok(version.to_owned())
}

/// Return the exact provenance compiled into a packaged binary.
///
/// # Errors
///
/// Developer binaries built outside the bundle command intentionally fail.
pub fn compiled_bundle_build_identity() -> Result<BundleBuildIdentity> {
    let product_commit = COMPILED_PRODUCT_COMMIT
        .ok_or_else(|| bundle_invalid("binary lacks packaged product-commit provenance"))?;
    let source_snapshot_sha256 = COMPILED_SOURCE_SNAPSHOT_SHA256
        .ok_or_else(|| bundle_invalid("binary lacks packaged source-snapshot provenance"))?;
    let source_state = compiled_source_state()?;
    let frontend_assets_sha256 = COMPILED_FRONTEND_ASSETS_SHA256
        .ok_or_else(|| bundle_invalid("binary lacks packaged frontend-asset provenance"))?;
    validate_product_commit(product_commit)?;
    validate_sha256(source_snapshot_sha256, "compiled source snapshot SHA-256")?;
    validate_sha256(frontend_assets_sha256, "compiled frontend asset SHA-256")?;
    Ok(BundleBuildIdentity {
        schema_version: "migration-stream/assisted-local-runtime/build-identity/v1".to_owned(),
        product_version: env!("CARGO_PKG_VERSION").to_owned(),
        product_commit: product_commit.to_owned(),
        source_state,
        source_snapshot_sha256: source_snapshot_sha256.to_owned(),
        frontend_assets_sha256: frontend_assets_sha256.to_owned(),
        target: supported_host_target()?,
    })
}

/// Build one no-replace runtime directory and optional deterministic ZIP.
///
/// # Errors
///
/// Rejects unsupported hosts, dirty provenance ambiguity, unsafe source
/// material, existing output names, and any result that fails independent
/// manifest replay.
// Keep the no-replace publication and rollback transaction contiguous for auditability.
#[allow(clippy::too_many_lines)]
pub fn assemble_bundle(request: &BundleAssemblyRequest) -> Result<BundleAssemblyResult> {
    let target = supported_host_target()?;
    if !target_bundle_assembly_enabled(&target) {
        return Err(KernelError::BundlePlatform(
            "Windows bundle assembly is disabled outside the explicit native acceptance-candidate lane"
                .to_owned(),
        ));
    }
    if !request.output_directory.is_absolute() {
        return Err(bundle_invalid(
            "bundle output directory must be one explicit absolute path",
        ));
    }
    validate_product_commit(&request.product_commit)?;
    validate_sha256(&request.source_snapshot_sha256, "source snapshot SHA-256")?;
    validate_sha256(&request.frontend_assets_sha256, "frontend asset SHA-256")?;
    if COMPILED_PRODUCT_COMMIT != Some(request.product_commit.as_str())
        || COMPILED_SOURCE_SNAPSHOT_SHA256 != Some(request.source_snapshot_sha256.as_str())
        || compiled_source_state()? != request.source_state
        || COMPILED_FRONTEND_ASSETS_SHA256 != Some(request.frontend_assets_sha256.as_str())
    {
        return Err(bundle_invalid(
            "bundle assembler was not built from the exact requested product commit, source state, and source snapshot",
        ));
    }
    let repository_root = require_real_directory(&request.repository_root, "repository root")?;
    verify_repository_source_authority(&repository_root, request)?;
    let frontend_assets_root =
        require_real_directory(&request.frontend_assets_root, "frontend asset staging root")?;
    if frontend_assets_root.starts_with(&repository_root) {
        return Err(bundle_invalid(
            "frontend assets must be built in one private root outside the repository",
        ));
    }
    verify_frontend_asset_snapshot(&frontend_assets_root, &request.frontend_assets_sha256)?;
    let release_binaries_root = require_private_staging_directory(
        &request.release_binaries_root,
        &repository_root,
        "release binary staging root",
    )?;
    let requested_output_parent = request
        .output_directory
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| bundle_invalid("bundle output has no usable parent"))?;
    let output_parent = require_real_directory(requested_output_parent, "bundle output parent")?;
    let output_name = request
        .output_directory
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| safe_component(value))
        .ok_or_else(|| bundle_invalid("bundle output name is unsafe"))?;
    let (output_directory, archive_path, archive_checksum_path) =
        canonical_bundle_output_paths(&output_parent, output_name);
    let requested_archive_path = request.output_directory.with_extension("zip");
    let requested_archive_checksum_path = request.output_directory.with_extension("zip.sha256");
    if path_lexically_exists(&output_directory)? {
        return Err(bundle_invalid("bundle output already exists"));
    }
    let output_paths = BTreeSet::from([
        output_directory.clone(),
        archive_path.clone(),
        archive_checksum_path.clone(),
    ]);
    let case_folded_output_names = output_paths
        .iter()
        .map(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .map(str::to_ascii_lowercase)
                .ok_or_else(|| bundle_invalid("bundle output name is not UTF-8"))
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if output_paths.len() != 3 || case_folded_output_names.len() != 3 {
        return Err(bundle_invalid(
            "bundle directory, archive, and checksum paths must be distinct",
        ));
    }
    if request.create_zip_archive
        && (path_lexically_exists(&archive_path)? || path_lexically_exists(&archive_checksum_path)?)
    {
        return Err(bundle_invalid(
            "bundle archive or checksum output already exists",
        ));
    }
    let partial = output_parent.join(format!(".{output_name}.part-{}", Uuid::new_v4()));
    let output_identity = create_owned_directory(&partial)?;

    let verification = match assemble_bundle_in_partial(
        &repository_root,
        &frontend_assets_root,
        &release_binaries_root,
        &partial,
        request,
        &target,
    ) {
        Ok(verification) => verification,
        Err(error) => {
            return Err(rollback_assembly_outputs(
                error,
                Some((partial.as_path(), &output_identity)),
                None,
                None,
            ));
        }
    };
    if let Err(failure) =
        publish_bundle_directory_noreplace(&partial, &output_directory, &output_identity)
    {
        let rollback_path = if failure.published {
            output_directory.as_path()
        } else {
            partial.as_path()
        };
        return Err(rollback_assembly_outputs(
            failure.error,
            Some((rollback_path, &output_identity)),
            None,
            None,
        ));
    }
    if let Err(error) = sync_directory(&output_parent) {
        return Err(rollback_assembly_outputs(
            error,
            Some((output_directory.as_path(), &output_identity)),
            None,
            None,
        ));
    }

    let final_verification =
        match verify_bundle_root_for_target(&output_directory, &verification.target) {
            Ok(verification) => verification,
            Err(error) => {
                return Err(rollback_assembly_outputs(
                    error,
                    Some((output_directory.as_path(), &output_identity)),
                    None,
                    None,
                ));
            }
        };
    if final_verification.manifest_sha256 != verification.manifest_sha256 {
        return Err(rollback_assembly_outputs(
            bundle_invalid("published bundle identity differs from the verified partial"),
            Some((output_directory.as_path(), &output_identity)),
            None,
            None,
        ));
    }

    let mut published_archive = None;
    let mut published_checksum = None;
    if request.create_zip_archive {
        let (archive, checksum) = match publish_archive_outputs(
            &output_directory,
            &archive_path,
            &archive_checksum_path,
            &final_verification,
        ) {
            Ok(outputs) => outputs,
            Err(error) => {
                return Err(rollback_assembly_outputs(
                    error,
                    Some((output_directory.as_path(), &output_identity)),
                    None,
                    None,
                ));
            }
        };
        published_archive = Some(archive);
        published_checksum = Some(checksum);
    }

    if let Err(error) =
        verify_repository_source_authority(&repository_root, request).and_then(|()| {
            verify_frontend_asset_snapshot(&frontend_assets_root, &request.frontend_assets_sha256)
        })
    {
        return Err(rollback_assembly_outputs(
            error,
            Some((output_directory.as_path(), &output_identity)),
            published_archive
                .as_ref()
                .map(|archive| (archive_path.as_path(), &archive.identity)),
            published_checksum
                .as_ref()
                .map(|identity| (archive_checksum_path.as_path(), identity)),
        ));
    }

    Ok(BundleAssemblyResult {
        bundle_directory: request.output_directory.clone(),
        manifest_sha256: final_verification.manifest_sha256,
        member_count: final_verification.member_count,
        total_member_bytes: final_verification.total_member_bytes,
        archive_path: published_archive.as_ref().map(|_| requested_archive_path),
        archive_sha256: published_archive.map(|archive| archive.sha256),
        archive_checksum_path: published_checksum.map(|_| requested_archive_checksum_path),
    })
}

#[derive(Debug)]
struct BundleDirectoryPublishFailure {
    error: KernelError,
    published: bool,
}

impl BundleDirectoryPublishFailure {
    fn before_move(error: KernelError) -> Self {
        Self {
            error,
            published: false,
        }
    }

    fn after_move(error: KernelError) -> Self {
        Self {
            error,
            published: true,
        }
    }
}

fn publish_bundle_directory_noreplace(
    source: &Path,
    destination: &Path,
    expected: &OwnedPathIdentity,
) -> std::result::Result<(), BundleDirectoryPublishFailure> {
    #[cfg(windows)]
    {
        publish_bundle_directory_noreplace_windows_with(
            source,
            destination,
            expected,
            WindowsBundleDirectoryPublishRetry {
                deadline: Instant::now() + WINDOWS_BUNDLE_DIRECTORY_PUBLISH_RETRY_WINDOW,
                maximum_attempts: WINDOWS_BUNDLE_DIRECTORY_PUBLISH_MAXIMUM_ATTEMPTS,
                retry_delay: WINDOWS_BUNDLE_DIRECTORY_PUBLISH_RETRY_DELAY,
            },
            crate::durability::publish_new,
        )
    }
    #[cfg(not(windows))]
    {
        rename_noreplace(source, destination)
            .map_err(BundleDirectoryPublishFailure::before_move)?;
        verify_owned_bundle_directory(
            destination,
            expected,
            "published bundle directory identity differs from the verified partial",
        )
        .map_err(BundleDirectoryPublishFailure::after_move)
    }
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug)]
struct WindowsBundleDirectoryPublishRetry {
    deadline: Instant,
    maximum_attempts: usize,
    retry_delay: Duration,
}

#[cfg(windows)]
fn publish_bundle_directory_noreplace_windows_with<F>(
    source: &Path,
    destination: &Path,
    expected: &OwnedPathIdentity,
    retry: WindowsBundleDirectoryPublishRetry,
    mut publish: F,
) -> std::result::Result<(), BundleDirectoryPublishFailure>
where
    F: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    let mut attempts = 0_usize;
    loop {
        let destination_observation = match observe_owned_bundle_directory(destination, expected) {
            Ok(observation) => observation,
            Err(error) if attempts == 0 => {
                return Err(BundleDirectoryPublishFailure::before_move(error));
            }
            Err(error) => return Err(BundleDirectoryPublishFailure::after_move(error)),
        };
        match destination_observation {
            OwnedBundleDirectoryObservation::Absent => {}
            OwnedBundleDirectoryObservation::Expected => {
                return Err(BundleDirectoryPublishFailure::after_move(bundle_invalid(
                    "exact bundle output appeared during no-replace publication",
                )));
            }
            OwnedBundleDirectoryObservation::Other => {
                return Err(BundleDirectoryPublishFailure::before_move(bundle_invalid(
                    "bundle output appeared during no-replace publication",
                )));
            }
        }
        verify_owned_bundle_directory(
            source,
            expected,
            "bundle publication refused a replaced partial directory",
        )
        .map_err(BundleDirectoryPublishFailure::before_move)?;
        attempts = attempts.saturating_add(1);
        match publish(source, destination) {
            Ok(()) => {
                return verify_owned_bundle_directory(
                    destination,
                    expected,
                    "published bundle directory identity differs from the verified partial",
                )
                .map_err(BundleDirectoryPublishFailure::after_move);
            }
            Err(error) => {
                let destination_observation = observe_owned_bundle_directory(destination, expected)
                    .map_err(BundleDirectoryPublishFailure::after_move)?;
                if destination_observation == OwnedBundleDirectoryObservation::Expected {
                    return Err(BundleDirectoryPublishFailure::after_move(KernelError::io(
                        destination,
                        error,
                    )));
                }
                if destination_observation == OwnedBundleDirectoryObservation::Other
                    || !windows_bundle_directory_publish_error_is_retryable(&error)
                    || attempts >= retry.maximum_attempts
                {
                    return Err(BundleDirectoryPublishFailure::before_move(KernelError::io(
                        destination,
                        error,
                    )));
                }
                let Some(remaining) = retry.deadline.checked_duration_since(Instant::now()) else {
                    return Err(BundleDirectoryPublishFailure::before_move(KernelError::io(
                        destination,
                        error,
                    )));
                };
                thread::sleep(remaining.min(retry.retry_delay));
                if Instant::now() >= retry.deadline {
                    return Err(BundleDirectoryPublishFailure::before_move(KernelError::io(
                        destination,
                        error,
                    )));
                }
            }
        }
    }
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnedBundleDirectoryObservation {
    Absent,
    Expected,
    Other,
}

#[cfg(windows)]
fn observe_owned_bundle_directory(
    path: &Path,
    expected: &OwnedPathIdentity,
) -> Result<OwnedBundleDirectoryObservation> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(OwnedBundleDirectoryObservation::Absent);
        }
        Err(error) => return Err(KernelError::io(path, error)),
    };
    if metadata.is_dir()
        && !metadata.file_type().is_symlink()
        && same_owned_directory_identity(path, &metadata, expected)?
    {
        Ok(OwnedBundleDirectoryObservation::Expected)
    } else {
        Ok(OwnedBundleDirectoryObservation::Other)
    }
}

fn verify_owned_bundle_directory(
    path: &Path,
    expected: &OwnedPathIdentity,
    mismatch_message: &'static str,
) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || !same_owned_directory_identity(path, &metadata, expected)?
    {
        return Err(bundle_invalid(mismatch_message));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_bundle_directory_publish_error_is_retryable(error: &std::io::Error) -> bool {
    // Windows reports transient image/current-directory/scanner handles with
    // one of these two namespace-lock errors. Permission and path failures
    // outside this exact set fail immediately.
    matches!(error.raw_os_error(), Some(5 | 32))
}

fn canonical_bundle_output_paths(
    canonical_output_parent: &Path,
    output_name: &str,
) -> (PathBuf, PathBuf, PathBuf) {
    let output_directory = canonical_output_parent.join(output_name);
    let archive_path = output_directory.with_extension("zip");
    let archive_checksum_path = output_directory.with_extension("zip.sha256");
    (output_directory, archive_path, archive_checksum_path)
}

// Keep construction of the complete canonical member graph in one auditable operation.
#[allow(clippy::too_many_lines)]
fn assemble_bundle_in_partial(
    repository_root: &Path,
    frontend_assets_root: &Path,
    release_binaries_root: &Path,
    partial: &Path,
    request: &BundleAssemblyRequest,
    target: &BundleTarget,
) -> Result<BundleVerification> {
    let specifications = [
        CopySpecification {
            source: release_binaries_root
                .join(binary_name_for(target, "migration-stream-local-runtime")),
            destination: runtime_controller_path_for(target),
            role: BundleMemberRole::RuntimeController,
        },
        CopySpecification {
            source: release_binaries_root.join(binary_name_for(target, "migration-stream")),
            destination: cli_path_for(target),
            role: BundleMemberRole::IntegrityCli,
        },
        CopySpecification {
            source: release_binaries_root
                .join(binary_name_for(target, "migration-stream-workbench")),
            destination: workbench_path_for(target),
            role: BundleMemberRole::Workbench,
        },
        CopySpecification {
            source: repository_root.join("config/engagement-intake-policy.json"),
            destination: INTAKE_POLICY_PATH,
            role: BundleMemberRole::Policy,
        },
        CopySpecification {
            source: repository_root.join("config/engagement-profiling-policy.json"),
            destination: PROFILING_POLICY_PATH,
            role: BundleMemberRole::Policy,
        },
        CopySpecification {
            source: repository_root.join("config/engagement-validation-policy.json"),
            destination: VALIDATION_POLICY_PATH,
            role: BundleMemberRole::Policy,
        },
        CopySpecification {
            source: repository_root.join("schemas/migration-object-profile.schema.json"),
            destination: PROFILE_SCHEMA_PATH,
            role: BundleMemberRole::ProfileSchema,
        },
        CopySpecification {
            source: repository_root.join("schemas/source-mapping-version.schema.json"),
            destination: MAPPING_SCHEMA_PATH,
            role: BundleMemberRole::MappingSchema,
        },
        CopySpecification {
            source: repository_root.join("schemas/assisted-runtime-bundle.schema.json"),
            destination: BUNDLE_SCHEMA_PATH,
            role: BundleMemberRole::BundleSchema,
        },
        CopySpecification {
            source: repository_root.join("docs/ASSISTED-LOCAL-RUNTIME.md"),
            destination: BUNDLE_OPERATOR_GUIDE_FILENAME,
            role: BundleMemberRole::OperatorGuide,
        },
    ];
    let mut members = Vec::new();
    if target.os == "windows" {
        copy_member(
            partial,
            &CopySpecification {
                source: release_binaries_root
                    .join(binary_name_for(target, "migration-stream-local-runtime")),
                destination: launcher_path_for(target),
                role: BundleMemberRole::PlatformLauncher,
            },
            &mut members,
        )?;
    } else {
        let launcher = platform_launcher_for(target)?;
        write_new_file(
            &partial.join(launcher_path_for(target)),
            launcher.as_bytes(),
            0o555,
        )?;
        members.push(member_from_bytes(
            launcher_path_for(target),
            BundleMemberRole::PlatformLauncher,
            launcher.as_bytes(),
        )?);
    }
    for specification in specifications {
        copy_member(partial, &specification, &mut members)?;
    }
    copy_tree(
        frontend_assets_root,
        partial,
        Path::new("web"),
        BundleMemberRole::FrontendAsset,
        &mut members,
    )?;
    for destination in CONTROLLED_PACK_PATHS {
        copy_member(
            partial,
            &CopySpecification {
                source: repository_root.join(destination),
                destination,
                role: BundleMemberRole::ControlledPack,
            },
            &mut members,
        )?;
    }
    verify_repository_source_authority(repository_root, request)?;
    verify_frontend_asset_snapshot(frontend_assets_root, &request.frontend_assets_sha256)?;
    members.sort_by(|left, right| left.path.cmp(&right.path));
    if members.len() > MAXIMUM_MEMBER_COUNT {
        return Err(bundle_invalid("bundle member count exceeds its bound"));
    }
    let mut manifest = AssistedRuntimeBundleManifest {
        schema_version: BUNDLE_FORMAT_VERSION.to_owned(),
        canonical_json_version: CANONICAL_JSON_VERSION.to_owned(),
        manifest_sha256: ZERO_SHA256.to_owned(),
        product: BundleProduct {
            name: env!("CARGO_PKG_NAME").to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            commit: request.product_commit.clone(),
            source_state: request.source_state,
            source_snapshot_sha256: request.source_snapshot_sha256.clone(),
        },
        target: target.clone(),
        entrypoints: BundleEntrypoints {
            launcher: launcher_path_for(target).to_owned(),
            runtime_controller: runtime_controller_path_for(target).to_owned(),
            integrity_cli: cli_path_for(target).to_owned(),
            workbench: workbench_path_for(target).to_owned(),
        },
        members,
    };
    manifest.manifest_sha256 = calculate_manifest_self_hash(&manifest)?;
    let manifest_bytes = canonical_json_bytes(&serde_json::to_value(&manifest)?)?;
    if u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX) > MAXIMUM_MANIFEST_BYTES {
        return Err(bundle_invalid("bundle manifest exceeds its byte bound"));
    }
    let manifest_path = partial.join(BUNDLE_MANIFEST_FILENAME);
    write_new_file(&manifest_path, &manifest_bytes, 0o444)?;
    seal_bundle_directories(partial)?;
    let verification = verify_bundle_root_for_target(partial, &manifest.target)?;
    verify_packaged_build_identities(partial, &compiled_bundle_build_identity()?)?;
    verify_packaged_launcher(partial, &verification)?;
    Ok(verification)
}

fn verify_packaged_build_identities(
    bundle_root: &Path,
    expected: &BundleBuildIdentity,
) -> Result<()> {
    for (path, arguments) in [
        (
            runtime_controller_path_for(&expected.target),
            &["__build-identity"][..],
        ),
        (
            cli_path_for(&expected.target),
            &["__bundle-build-identity"][..],
        ),
        (
            workbench_path_for(&expected.target),
            &["--bundle-build-identity"][..],
        ),
    ] {
        let executable = bundle_root.join(path);
        let output = Command::new(&executable)
            .args(arguments)
            .current_dir(bundle_root)
            .output()
            .map_err(|error| KernelError::io(&executable, error))?;
        if !output.status.success()
            || output.stdout.is_empty()
            || output.stdout.len() > 128 * 1024
            || output.stderr.len() > 128 * 1024
        {
            return Err(bundle_invalid(format!(
                "packaged binary {path} did not provide bounded build identity"
            )));
        }
        let value = parse_unique_json(&output.stdout).map_err(|error| {
            bundle_invalid(format!(
                "packaged binary {path} identity is invalid: {error}"
            ))
        })?;
        if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err(bundle_invalid(format!(
                "packaged binary {path} rejected its build identity"
            )));
        }
        let identity: BundleBuildIdentity = serde_json::from_value(
            value
                .get("result")
                .cloned()
                .ok_or_else(|| bundle_invalid("packaged build identity result is absent"))?,
        )
        .map_err(|error| {
            bundle_invalid(format!(
                "packaged binary {path} identity is invalid: {error}"
            ))
        })?;
        if &identity != expected {
            return Err(bundle_invalid(format!(
                "packaged binary {path} was not built from the exact bundle source"
            )));
        }
    }
    Ok(())
}

fn verify_packaged_launcher(bundle_root: &Path, verification: &BundleVerification) -> Result<()> {
    let launcher = bundle_root.join(launcher_path_for(&verification.target));
    let output = Command::new(&launcher)
        .arg("verify")
        .current_dir(bundle_root)
        .output()
        .map_err(|error| KernelError::io(&launcher, error))?;
    if !output.status.success()
        || output.stdout.is_empty()
        || output.stdout.len() > 256 * 1024
        || output.stderr.len() > 128 * 1024
    {
        return Err(bundle_invalid(
            "packaged platform launcher could not verify the exact bundle",
        ));
    }
    let value = parse_unique_json(&output.stdout)
        .map_err(|error| bundle_invalid(format!("launcher verification is invalid: {error}")))?;
    let observed = value
        .pointer("/result/manifestSha256")
        .and_then(serde_json::Value::as_str);
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true)
        || observed != Some(verification.manifest_sha256.as_str())
    {
        return Err(bundle_invalid(
            "packaged platform launcher reported a different bundle identity",
        ));
    }
    Ok(())
}

fn copy_member(
    bundle_root: &Path,
    specification: &CopySpecification,
    members: &mut Vec<BundleMember>,
) -> Result<()> {
    validate_member_path(specification.destination)?;
    let bytes = read_regular_file_bounded(&specification.source, MAXIMUM_MEMBER_BYTES)?;
    let destination = bundle_root.join(specification.destination);
    if let Some(parent) = destination.parent() {
        create_directory_tree(bundle_root, parent)?;
    }
    write_new_file(
        &destination,
        &bytes,
        parse_mode(specification.role.expected_mode())?,
    )?;
    members.push(member_from_bytes(
        specification.destination,
        specification.role,
        &bytes,
    )?);
    Ok(())
}

fn copy_tree(
    source_root: &Path,
    bundle_root: &Path,
    destination_root: &Path,
    role: BundleMemberRole,
    members: &mut Vec<BundleMember>,
) -> Result<()> {
    let source_root = require_real_directory(source_root, "bundle source tree")?;
    let destination = bundle_root.join(destination_root);
    create_directory_tree(bundle_root, &destination)?;
    let mut progress = TreeCopyProgress::default();
    copy_tree_directory(
        &source_root,
        &source_root,
        bundle_root,
        destination_root,
        role,
        members,
        &mut progress,
    )?;
    if progress.files == 0 {
        return Err(bundle_invalid("bundle frontend asset tree is empty"));
    }
    Ok(())
}

fn copy_tree_directory(
    source_root: &Path,
    directory: &Path,
    bundle_root: &Path,
    destination_root: &Path,
    role: BundleMemberRole,
    members: &mut Vec<BundleMember>,
    progress: &mut TreeCopyProgress,
) -> Result<()> {
    for entry in fs::read_dir(directory).map_err(|error| KernelError::io(directory, error))? {
        let entry = entry.map_err(|error| KernelError::io(directory, error))?;
        progress.entries = progress
            .entries
            .checked_add(1)
            .ok_or_else(|| bundle_invalid("frontend asset graph count overflow"))?;
        if progress.entries > MAXIMUM_FRONTEND_GRAPH_ENTRY_COUNT {
            return Err(bundle_invalid(
                "frontend asset graph exceeds its entry bound",
            ));
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| bundle_invalid("bundle source path is not UTF-8"))?;
        if name.starts_with('.') || !safe_component(&name) {
            return Err(bundle_invalid(format!(
                "bundle source contains unsafe hidden or special name {name:?}"
            )));
        }
        let source = entry.path();
        let metadata =
            fs::symlink_metadata(&source).map_err(|error| KernelError::io(&source, error))?;
        if metadata.file_type().is_symlink() {
            return Err(bundle_invalid(format!(
                "bundle source contains link {}",
                source.display()
            )));
        }
        let relative = source
            .strip_prefix(source_root)
            .map_err(|_| bundle_invalid("bundle source escaped its root"))?;
        let destination_relative = destination_root.join(relative);
        let destination_path = path_to_manifest_string(&destination_relative)?;
        if metadata.is_dir() {
            create_directory_tree(bundle_root, &bundle_root.join(&destination_relative))?;
            copy_tree_directory(
                source_root,
                &source,
                bundle_root,
                destination_root,
                role,
                members,
                progress,
            )?;
        } else if metadata.is_file() {
            progress.files = progress
                .files
                .checked_add(1)
                .ok_or_else(|| bundle_invalid("frontend asset file count overflow"))?;
            if progress.files > MAXIMUM_FRONTEND_MEMBER_COUNT
                || metadata.len() > MAXIMUM_FRONTEND_MEMBER_BYTES
            {
                return Err(bundle_invalid("frontend asset file bounds were exceeded"));
            }
            progress.bytes = progress
                .bytes
                .checked_add(metadata.len())
                .ok_or_else(|| bundle_invalid("frontend asset byte count overflow"))?;
            if progress.bytes > MAXIMUM_FRONTEND_TOTAL_BYTES {
                return Err(bundle_invalid("frontend asset bytes exceed their bound"));
            }
            let bytes = read_regular_file_bounded(&source, MAXIMUM_FRONTEND_MEMBER_BYTES)?;
            let destination = bundle_root.join(&destination_relative);
            write_new_file(&destination, &bytes, parse_mode(role.expected_mode())?)?;
            members.push(member_from_bytes(&destination_path, role, &bytes)?);
        } else {
            return Err(bundle_invalid(format!(
                "bundle source contains unsupported file type {}",
                source.display()
            )));
        }
    }
    Ok(())
}

fn member_from_bytes(path: &str, role: BundleMemberRole, bytes: &[u8]) -> Result<BundleMember> {
    validate_member_path(path)?;
    Ok(BundleMember {
        path: path.to_owned(),
        member_type: BundleMemberType::RegularFile,
        role,
        mode: role.expected_mode().to_owned(),
        size: u64::try_from(bytes.len())
            .map_err(|_| bundle_invalid("bundle member size exceeds u64"))?,
        sha256: sha256_hex(bytes),
    })
}

/// Verify the complete bundle on the current supported host.
///
/// # Errors
///
/// Rejects platform mismatch, noncanonical or self-hash-mismatched manifest
/// bytes, unsafe names/types/modes/links, missing or extra members, and any
/// size/hash mismatch.
pub fn verify_bundle_root(root: impl AsRef<Path>) -> Result<BundleVerification> {
    let target = supported_host_target()?;
    verify_bundle_root_for_target(root.as_ref(), &target)
}

/// Verify a bundle against one exact expected target.
///
/// This separate entry is used by format-level tests; the shipped launcher
/// always calls [`verify_bundle_root`] and therefore cannot override its host.
///
/// # Errors
///
/// Returns the same fail-closed errors as [`verify_bundle_root`].
pub fn verify_bundle_root_for_target(
    root: impl AsRef<Path>,
    expected_target: &BundleTarget,
) -> Result<BundleVerification> {
    let root = require_real_directory(root.as_ref(), "bundle root")?;
    verify_directory_mode(&root)?;
    let manifest_path = root.join(BUNDLE_MANIFEST_FILENAME);
    let manifest_bytes = read_regular_file_bounded(&manifest_path, MAXIMUM_MANIFEST_BYTES)?;
    verify_file_mode(&manifest_path, "0444")?;
    let manifest_value = parse_unique_json(&manifest_bytes)
        .map_err(|error| bundle_invalid(format!("manifest JSON is invalid: {error}")))?;
    if canonical_json_bytes(&manifest_value)? != manifest_bytes {
        return Err(bundle_invalid("manifest JSON is not canonical"));
    }
    let manifest: AssistedRuntimeBundleManifest = serde_json::from_value(manifest_value)
        .map_err(|error| bundle_invalid(format!("manifest contract is invalid: {error}")))?;
    validate_manifest(&manifest, expected_target)?;

    let expected_directories = expected_directory_set(&manifest)?;
    let actual = collect_actual_bundle_graph(
        &root,
        manifest.members.len() + 1,
        expected_directories.len(),
    )?;
    if actual.directories != expected_directories {
        return Err(bundle_invalid(
            "bundle directory graph contains missing or undeclared paths",
        ));
    }
    let expected_paths = manifest
        .members
        .iter()
        .map(|member| member.path.clone())
        .collect::<BTreeSet<_>>();
    if actual.files != expected_paths {
        return Err(bundle_invalid(
            "bundle contains missing or undeclared regular files",
        ));
    }

    let mut total_member_bytes = 0_u64;
    for member in &manifest.members {
        let path = root.join(&member.path);
        verify_file_mode(&path, &member.mode)?;
        let (size, hash) = hash_regular_file_bounded(&path, MAXIMUM_MEMBER_BYTES)?;
        if size != member.size {
            return Err(bundle_invalid(format!(
                "bundle member {} size differs from its manifest",
                member.path
            )));
        }
        if hash != member.sha256 {
            return Err(bundle_invalid(format!(
                "bundle member {} hash differs from its manifest",
                member.path
            )));
        }
        if matches!(
            member.role,
            BundleMemberRole::RuntimeController
                | BundleMemberRole::IntegrityCli
                | BundleMemberRole::Workbench
        ) || (member.role == BundleMemberRole::PlatformLauncher
            && expected_target.os == "windows")
        {
            verify_platform_executable(&path, expected_target)?;
        }
        total_member_bytes = total_member_bytes
            .checked_add(size)
            .ok_or_else(|| bundle_invalid("bundle total size overflow"))?;
        if total_member_bytes > MAXIMUM_BUNDLE_BYTES {
            return Err(bundle_invalid("bundle total size exceeds its bound"));
        }
    }

    let launcher_sha256 = member_hash(&manifest, launcher_path_for(expected_target))?;
    let runtime_controller_sha256 =
        member_hash(&manifest, runtime_controller_path_for(expected_target))?;
    let integrity_cli_sha256 = member_hash(&manifest, cli_path_for(expected_target))?;
    let workbench_sha256 = member_hash(&manifest, workbench_path_for(expected_target))?;
    Ok(BundleVerification {
        bundle_root: root,
        manifest_sha256: manifest.manifest_sha256,
        product_commit: manifest.product.commit,
        product_version: manifest.product.version,
        source_state: manifest.product.source_state,
        source_snapshot_sha256: manifest.product.source_snapshot_sha256,
        target: manifest.target,
        launcher_sha256,
        runtime_controller_sha256,
        integrity_cli_sha256,
        workbench_sha256,
        member_count: manifest.members.len(),
        total_member_bytes,
    })
}

fn member_hash(manifest: &AssistedRuntimeBundleManifest, path: &str) -> Result<String> {
    manifest
        .members
        .iter()
        .find(|member| member.path == path)
        .map(|member| member.sha256.clone())
        .ok_or_else(|| bundle_invalid(format!("manifest lacks required member {path}")))
}

// Keep the independent fail-closed manifest authority replay contiguous for auditability.
#[allow(clippy::too_many_lines)]
fn validate_manifest(
    manifest: &AssistedRuntimeBundleManifest,
    expected_target: &BundleTarget,
) -> Result<()> {
    if manifest.schema_version != BUNDLE_FORMAT_VERSION
        || manifest.canonical_json_version != CANONICAL_JSON_VERSION
    {
        return Err(bundle_invalid(
            "manifest schema or canonical JSON version is unsupported",
        ));
    }
    validate_sha256(&manifest.manifest_sha256, "manifest self-checksum")?;
    if manifest.manifest_sha256 == ZERO_SHA256
        || calculate_manifest_self_hash(manifest)? != manifest.manifest_sha256
    {
        return Err(bundle_invalid("manifest self-checksum does not match"));
    }
    if manifest.product.name != env!("CARGO_PKG_NAME")
        || manifest.product.version != env!("CARGO_PKG_VERSION")
        || manifest.product.version.len() > MAXIMUM_PRODUCT_VERSION_BYTES
    {
        return Err(bundle_invalid("manifest product identity is invalid"));
    }
    validate_product_commit(&manifest.product.commit)?;
    validate_sha256(
        &manifest.product.source_snapshot_sha256,
        "source snapshot SHA-256",
    )?;
    if !cfg!(test) {
        let compiled_commit = COMPILED_PRODUCT_COMMIT.ok_or_else(|| {
            bundle_invalid("runtime controller lacks packaged product-commit provenance")
        })?;
        let compiled_snapshot = COMPILED_SOURCE_SNAPSHOT_SHA256.ok_or_else(|| {
            bundle_invalid("runtime controller lacks packaged source-snapshot provenance")
        })?;
        let compiled_source_state = compiled_source_state()?;
        let compiled_frontend_assets = COMPILED_FRONTEND_ASSETS_SHA256.ok_or_else(|| {
            bundle_invalid("runtime controller lacks packaged frontend-asset provenance")
        })?;
        if compiled_commit != manifest.product.commit
            || compiled_snapshot != manifest.product.source_snapshot_sha256
            || compiled_source_state != manifest.product.source_state
            || compiled_frontend_assets != frontend_asset_manifest_sha256(manifest)?
        {
            return Err(bundle_invalid(
                "manifest product provenance differs from the compiled runtime controller",
            ));
        }
    }
    if &manifest.target != expected_target {
        return Err(KernelError::BundlePlatform(format!(
            "bundle target {}-{} ({}) differs from required {}-{} ({})",
            manifest.target.os,
            manifest.target.architecture,
            manifest.target.target_triple,
            expected_target.os,
            expected_target.architecture,
            expected_target.target_triple
        )));
    }
    let declared_target = target_for(&manifest.target.os, &manifest.target.architecture)?;
    if manifest.target != declared_target {
        return Err(bundle_invalid(
            "manifest target triple or platform baseline differs from the canonical matrix",
        ));
    }
    if manifest.entrypoints.launcher != launcher_path_for(expected_target)
        || manifest.entrypoints.runtime_controller != runtime_controller_path_for(expected_target)
        || manifest.entrypoints.integrity_cli != cli_path_for(expected_target)
        || manifest.entrypoints.workbench != workbench_path_for(expected_target)
    {
        return Err(bundle_invalid(
            "manifest entrypoints are not the fixed target-specific paths",
        ));
    }
    if manifest.members.is_empty() || manifest.members.len() > MAXIMUM_MEMBER_COUNT {
        return Err(bundle_invalid("manifest member count is outside its bound"));
    }
    let mut previous = None;
    let mut roles = BTreeMap::<BundleMemberRoleKey, usize>::new();
    let mut case_folded_paths = BTreeSet::new();
    for member in &manifest.members {
        validate_member_path(&member.path)?;
        if !case_folded_paths.insert(member.path.to_ascii_lowercase()) {
            return Err(bundle_invalid(
                "manifest member paths collide on a case-insensitive filesystem",
            ));
        }
        if member.path == BUNDLE_MANIFEST_FILENAME {
            return Err(bundle_invalid(
                "the root manifest is self-checksummed and cannot list itself",
            ));
        }
        if previous.as_ref().is_some_and(|value| value >= &member.path) {
            return Err(bundle_invalid(
                "manifest members are not strictly path-sorted and unique",
            ));
        }
        previous = Some(member.path.clone());
        if member.member_type != BundleMemberType::RegularFile
            || member.mode != member.role.expected_mode()
            || member.size > member.role.maximum_bytes()
        {
            return Err(bundle_invalid(format!(
                "manifest member {} has invalid type, mode, or size",
                member.path
            )));
        }
        validate_sha256(&member.sha256, "bundle member SHA-256")?;
        *roles
            .entry(BundleMemberRoleKey::from(member.role))
            .or_default() += 1;
    }
    if roles
        .get(&BundleMemberRoleKey::FrontendAsset)
        .copied()
        .unwrap_or_default()
        > MAXIMUM_FRONTEND_MEMBER_COUNT
    {
        return Err(bundle_invalid(
            "manifest frontend member count exceeds the workbench bound",
        ));
    }
    let frontend_total = manifest
        .members
        .iter()
        .filter(|member| member.role == BundleMemberRole::FrontendAsset)
        .try_fold(0_u64, |total, member| total.checked_add(member.size))
        .ok_or_else(|| bundle_invalid("manifest frontend size overflow"))?;
    if frontend_total > MAXIMUM_FRONTEND_TOTAL_BYTES {
        return Err(bundle_invalid(
            "manifest frontend bytes exceed the workbench bound",
        ));
    }
    require_exact_role_path(
        manifest,
        BundleMemberRole::PlatformLauncher,
        launcher_path_for(expected_target),
        "platform launcher",
    )?;
    require_exact_role_path(
        manifest,
        BundleMemberRole::RuntimeController,
        runtime_controller_path_for(expected_target),
        "runtime controller",
    )?;
    require_exact_role_path(
        manifest,
        BundleMemberRole::IntegrityCli,
        cli_path_for(expected_target),
        "integrity CLI",
    )?;
    require_exact_role_path(
        manifest,
        BundleMemberRole::Workbench,
        workbench_path_for(expected_target),
        "workbench",
    )?;
    require_exact_role_path(
        manifest,
        BundleMemberRole::ProfileSchema,
        PROFILE_SCHEMA_PATH,
        "profile schema",
    )?;
    require_exact_role_path(
        manifest,
        BundleMemberRole::MappingSchema,
        MAPPING_SCHEMA_PATH,
        "mapping schema",
    )?;
    require_exact_role_path(
        manifest,
        BundleMemberRole::BundleSchema,
        BUNDLE_SCHEMA_PATH,
        "bundle schema",
    )?;
    require_exact_role_path(
        manifest,
        BundleMemberRole::OperatorGuide,
        BUNDLE_OPERATOR_GUIDE_FILENAME,
        "operator guide",
    )?;
    for path in [
        INTAKE_POLICY_PATH,
        PROFILING_POLICY_PATH,
        VALIDATION_POLICY_PATH,
    ] {
        if !manifest
            .members
            .iter()
            .any(|member| member.path == path && member.role == BundleMemberRole::Policy)
        {
            return Err(bundle_invalid(format!(
                "manifest lacks required policy {path}"
            )));
        }
    }
    let policy_paths = manifest
        .members
        .iter()
        .filter(|member| member.role == BundleMemberRole::Policy)
        .map(|member| member.path.as_str())
        .collect::<BTreeSet<_>>();
    if policy_paths
        != [
            INTAKE_POLICY_PATH,
            PROFILING_POLICY_PATH,
            VALIDATION_POLICY_PATH,
        ]
        .into_iter()
        .collect()
    {
        return Err(bundle_invalid(
            "manifest policy inventory differs from the exact runtime inventory",
        ));
    }
    if !manifest.members.iter().any(|member| {
        member.path == FRONTEND_INDEX_PATH && member.role == BundleMemberRole::FrontendAsset
    }) {
        return Err(bundle_invalid("manifest lacks the frontend index"));
    }
    if roles
        .get(&BundleMemberRoleKey::ControlledPack)
        .copied()
        .unwrap_or_default()
        == 0
    {
        return Err(bundle_invalid("manifest lacks controlled object packs"));
    }
    let controlled_pack_paths = manifest
        .members
        .iter()
        .filter(|member| member.role == BundleMemberRole::ControlledPack)
        .map(|member| member.path.as_str())
        .collect::<BTreeSet<_>>();
    if controlled_pack_paths != CONTROLLED_PACK_PATHS.into_iter().collect() {
        return Err(bundle_invalid(
            "manifest controlled-pack inventory differs from the exact runtime inventory",
        ));
    }
    let launcher_hash = member_hash(manifest, launcher_path_for(expected_target))?;
    if expected_target.os == "windows" {
        if launcher_hash != member_hash(manifest, runtime_controller_path_for(expected_target))? {
            return Err(bundle_invalid(
                "Windows command launcher is not the exact native runtime controller",
            ));
        }
    } else if launcher_hash != sha256_hex(platform_launcher_for(expected_target)?.as_bytes()) {
        return Err(bundle_invalid(
            "platform launcher bytes differ from the fixed target-specific launcher",
        ));
    }
    Ok(())
}

fn compiled_source_state() -> Result<BundleSourceState> {
    match COMPILED_SOURCE_STATE {
        Some("clean") => Ok(BundleSourceState::Clean),
        Some("explicit-uncommitted-snapshot") => Ok(BundleSourceState::ExplicitUncommittedSnapshot),
        Some(_) => Err(bundle_invalid(
            "binary has unsupported packaged source-state provenance",
        )),
        None => Err(bundle_invalid(
            "binary lacks packaged source-state provenance",
        )),
    }
}

/// A retained, explicitly selected Git executable used only to replay the
/// repository authority that is bound into a bundle.  The build driver is
/// responsible for resolving the executable; the assembler deliberately
/// never falls back to PATH.
struct VerifiedGitExecutable {
    path: PathBuf,
    file: File,
    metadata: fs::Metadata,
    #[cfg(windows)]
    native_file: migration_stream_windows_native::RealFile,
}

fn verify_git_executable(path: &Path) -> Result<VerifiedGitExecutable> {
    if !path.is_absolute() {
        return Err(bundle_invalid(
            "Git executable must be one explicit absolute path",
        ));
    }
    #[cfg(windows)]
    let native_file = migration_stream_windows_native::RealFile::capture(path)
        .map_err(|error| KernelError::io(path, error))?;
    // RealFile normalizes its retained authority path to a volume GUID. That
    // is the correct form for identity replay, but CreateProcess rejects it
    // for Git for Windows. Keep the caller's verified DOS path solely for
    // execution while RealFile retains and replays the normalized authority.
    let before = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(bundle_invalid(
            "Git executable must be one real regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if before.mode() & 0o111 == 0 {
            return Err(bundle_invalid("Git executable is not executable"));
        }
    }
    #[cfg(windows)]
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("exe"))
    {
        return Err(bundle_invalid("Git executable must be one .exe file"));
    }
    let file = File::open(path).map_err(|error| KernelError::io(path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    if !same_file_identity(&before, &metadata) {
        return Err(bundle_invalid("Git executable changed while opening"));
    }
    Ok(VerifiedGitExecutable {
        path: path.to_path_buf(),
        file,
        metadata,
        #[cfg(windows)]
        native_file,
    })
}

impl VerifiedGitExecutable {
    fn verify_named_identity(&self) -> Result<()> {
        verify_named_open_identity_allow_hard_links(&self.path, &self.file, &self.metadata)?;
        #[cfg(windows)]
        self.native_file
            .verify()
            .map_err(|error| KernelError::io(&self.path, error))?;
        Ok(())
    }
}

fn verify_repository_source_authority(
    repository_root: &Path,
    request: &BundleAssemblyRequest,
) -> Result<()> {
    let git = verify_git_executable(&request.git_executable)?;
    let head_before = repository_head(repository_root, &git)?;
    let state_before = repository_source_state(repository_root, &git)?;
    let observed_snapshot = repository_source_snapshot_sha256(repository_root, &git)?;
    let state_after = repository_source_state(repository_root, &git)?;
    let head_after = repository_head(repository_root, &git)?;
    if head_before != request.product_commit || head_after != request.product_commit {
        return Err(bundle_invalid(
            "repository HEAD changed during bundle assembly",
        ));
    }
    if state_before != request.source_state || state_after != request.source_state {
        return Err(bundle_invalid(
            "repository clean/dirty classification changed during bundle assembly",
        ));
    }
    if observed_snapshot != request.source_snapshot_sha256 {
        return Err(bundle_invalid(
            "Git-visible source bytes changed during bundle assembly",
        ));
    }
    Ok(())
}

fn repository_head(repository_root: &Path, git: &VerifiedGitExecutable) -> Result<String> {
    let output = git_output(git, repository_root, &["rev-parse", "--verify", "HEAD"])?;
    if !output.status.success() || !output.stderr.is_empty() || output.stdout.len() > 64 {
        return Err(bundle_invalid("repository HEAD could not be replayed"));
    }
    let head = std::str::from_utf8(&output.stdout)
        .map_err(|_| bundle_invalid("repository HEAD is not UTF-8"))?
        .trim();
    validate_product_commit(head)?;
    Ok(head.to_owned())
}

fn repository_source_state(
    repository_root: &Path,
    git: &VerifiedGitExecutable,
) -> Result<BundleSourceState> {
    let staged = git_output(
        git,
        repository_root,
        &["diff", "--cached", "--quiet", "--exit-code"],
    )?;
    if staged.status.code() != Some(0) || !staged.stdout.is_empty() || !staged.stderr.is_empty() {
        return Err(bundle_invalid(
            "staged or unreadable source changes are ambiguous",
        ));
    }
    let unmerged = git_output(git, repository_root, &["ls-files", "--unmerged", "-z"])?;
    if !unmerged.status.success() || !unmerged.stdout.is_empty() || !unmerged.stderr.is_empty() {
        return Err(bundle_invalid("unmerged source paths are not supported"));
    }
    let status = git_output(
        git,
        repository_root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )?;
    if !status.status.success()
        || !status.stderr.is_empty()
        || status.stdout.len() > MAXIMUM_SOURCE_INVENTORY_BYTES
    {
        return Err(bundle_invalid(
            "repository source status could not be classified within its bound",
        ));
    }
    Ok(if status.stdout.is_empty() {
        BundleSourceState::Clean
    } else {
        BundleSourceState::ExplicitUncommittedSnapshot
    })
}

fn git_output(
    git: &VerifiedGitExecutable,
    repository_root: &Path,
    arguments: &[&str],
) -> Result<std::process::Output> {
    git.verify_named_identity()?;
    let output = Command::new(&git.path)
        .arg("-C")
        .arg(repository_root)
        .args(arguments)
        .output()
        .map_err(|error| KernelError::io(&git.path, error))?;
    git.verify_named_identity()?;
    Ok(output)
}

fn repository_source_snapshot_sha256(
    repository_root: &Path,
    git: &VerifiedGitExecutable,
) -> Result<String> {
    let output = git_output(
        git,
        repository_root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    if !output.status.success()
        || !output.stderr.is_empty()
        || output.stdout.len() > MAXIMUM_SOURCE_INVENTORY_BYTES
    {
        return Err(bundle_invalid(
            "Git-visible source inventory could not be read within its bound",
        ));
    }
    let mut paths = output.stdout.split(|byte| *byte == 0).collect::<Vec<_>>();
    if paths.last().is_some_and(|value| value.is_empty()) {
        paths.pop();
    }
    if paths.is_empty() || paths.len() > MAXIMUM_SOURCE_FILE_COUNT {
        return Err(bundle_invalid(
            "Git-visible source inventory count is outside its bound",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut files = Vec::with_capacity(paths.len());
    let mut total_bytes = 0_u64;
    for raw_path in paths {
        let relative = std::str::from_utf8(raw_path)
            .map_err(|_| bundle_invalid("Git-visible source path is not UTF-8"))?;
        validate_source_path(relative)?;
        if !seen.insert(relative.to_owned()) {
            return Err(bundle_invalid(
                "Git-visible source inventory contains duplicates",
            ));
        }
        let path = repository_root.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Explicit dirty snapshots may deliberately delete tracked
                // paths. The repeated state and snapshot replay binds that
                // absence without following a dangling link.
                continue;
            }
            Err(error) => return Err(KernelError::io(&path, error)),
        }
        reject_source_path_links(repository_root, &path)?;
        let (bytes, sha256, metadata) = hash_source_file_bounded(&path)?;
        total_bytes = total_bytes
            .checked_add(bytes)
            .ok_or_else(|| bundle_invalid("source snapshot total size overflow"))?;
        if total_bytes > MAXIMUM_SOURCE_TOTAL_BYTES {
            return Err(bundle_invalid(
                "source snapshot total size exceeds its bound",
            ));
        }
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::MetadataExt as _;
            metadata.mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = {
            let _ = metadata;
            false
        };
        files.push(SourceSnapshotMember {
            path: relative.to_owned(),
            executable,
            bytes,
            sha256,
        });
    }
    files.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let manifest = SourceSnapshotManifest {
        schema_version: "migration-stream/source-snapshot-manifest/v1",
        files,
    };
    Ok(sha256_hex(canonical_json_bytes(&serde_json::to_value(
        manifest,
    )?)?))
}

fn validate_source_path(value: &str) -> Result<()> {
    if value.is_empty()
        || value.starts_with('/')
        || value.bytes().any(|byte| byte < 32 || byte == 127)
        || value
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(bundle_invalid(format!(
            "Git-visible source path {value:?} is unsafe"
        )));
    }
    Ok(())
}

fn reject_source_path_links(repository_root: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(repository_root)
        .map_err(|_| bundle_invalid("Git-visible source path escaped the repository"))?;
    let mut current = repository_root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(bundle_invalid("Git-visible source path is not normalized"));
        };
        current.push(component);
        let metadata =
            fs::symlink_metadata(&current).map_err(|error| KernelError::io(&current, error))?;
        if metadata.file_type().is_symlink() {
            return Err(bundle_invalid(format!(
                "Git-visible source path contains a symbolic link: {}",
                relative.display()
            )));
        }
    }
    Ok(())
}

fn hash_source_file_bounded(path: &Path) -> Result<(u64, String, fs::Metadata)> {
    let before = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !before.is_file()
        || before.file_type().is_symlink()
        || before.len() > MAXIMUM_SOURCE_FILE_BYTES
    {
        return Err(bundle_invalid(format!(
            "Git-visible source {} is not a bounded regular file",
            path.display()
        )));
    }
    let mut file = File::open(path).map_err(|error| KernelError::io(path, error))?;
    let opened = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    if !same_file_identity(&before, &opened) {
        return Err(bundle_invalid(format!(
            "Git-visible source {} changed while opening",
            path.display()
        )));
    }
    let (bytes, sha256) = hash_open_file_bounded(&mut file, path, MAXIMUM_SOURCE_FILE_BYTES)?;
    let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !named.is_file()
        || named.file_type().is_symlink()
        || !same_file_identity(&opened, &named)
        || named.len() != bytes
    {
        return Err(bundle_invalid(format!(
            "Git-visible source {} changed while hashing",
            path.display()
        )));
    }
    Ok((bytes, sha256, opened))
}

fn verify_frontend_asset_snapshot(root: &Path, expected_sha256: &str) -> Result<()> {
    let observed = frontend_asset_snapshot_sha256(root)?;
    if observed != expected_sha256 {
        return Err(bundle_invalid(
            "frontend asset bytes changed during bundle assembly",
        ));
    }
    Ok(())
}

fn frontend_asset_snapshot_sha256(root: &Path) -> Result<String> {
    let root = require_real_directory(root, "frontend asset staging root")?;
    let mut progress = TreeCopyProgress::default();
    let mut files = Vec::new();
    collect_frontend_asset_snapshot(&root, &root, &mut progress, &mut files)?;
    if files.is_empty() {
        return Err(bundle_invalid("frontend asset staging root is empty"));
    }
    files.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let manifest = FrontendAssetSnapshotManifest {
        schema_version: "migration-stream/frontend-asset-snapshot/v1",
        files,
    };
    Ok(sha256_hex(canonical_json_bytes(&serde_json::to_value(
        manifest,
    )?)?))
}

fn collect_frontend_asset_snapshot(
    root: &Path,
    directory: &Path,
    progress: &mut TreeCopyProgress,
    files: &mut Vec<FrontendAssetSnapshotMember>,
) -> Result<()> {
    for entry in fs::read_dir(directory).map_err(|error| KernelError::io(directory, error))? {
        let entry = entry.map_err(|error| KernelError::io(directory, error))?;
        progress.entries = progress
            .entries
            .checked_add(1)
            .ok_or_else(|| bundle_invalid("frontend asset graph count overflow"))?;
        if progress.entries > MAXIMUM_FRONTEND_GRAPH_ENTRY_COUNT {
            return Err(bundle_invalid(
                "frontend asset graph exceeds its entry bound",
            ));
        }
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| bundle_invalid("frontend asset path escaped its staging root"))?;
        let relative = path_to_manifest_string(relative)?;
        validate_member_path(&format!("web/{relative}"))?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| KernelError::io(&path, error))?;
        if metadata.file_type().is_symlink() {
            return Err(bundle_invalid("frontend asset graph contains a link"));
        }
        if metadata.is_dir() {
            collect_frontend_asset_snapshot(root, &path, progress, files)?;
        } else if metadata.is_file() {
            progress.files = progress
                .files
                .checked_add(1)
                .ok_or_else(|| bundle_invalid("frontend asset file count overflow"))?;
            if progress.files > MAXIMUM_FRONTEND_MEMBER_COUNT
                || metadata.len() > MAXIMUM_FRONTEND_MEMBER_BYTES
            {
                return Err(bundle_invalid("frontend asset file bounds were exceeded"));
            }
            progress.bytes = progress
                .bytes
                .checked_add(metadata.len())
                .ok_or_else(|| bundle_invalid("frontend asset byte count overflow"))?;
            if progress.bytes > MAXIMUM_FRONTEND_TOTAL_BYTES {
                return Err(bundle_invalid("frontend asset bytes exceed their bound"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt as _;
                if metadata.mode() & 0o111 != 0 {
                    return Err(bundle_invalid("frontend asset must not be executable"));
                }
            }
            let (bytes, sha256) = hash_regular_file_bounded(&path, MAXIMUM_FRONTEND_MEMBER_BYTES)?;
            files.push(FrontendAssetSnapshotMember {
                path: relative,
                bytes,
                sha256,
            });
        } else {
            return Err(bundle_invalid(
                "frontend asset graph contains an unsupported file type",
            ));
        }
    }
    Ok(())
}

fn frontend_asset_manifest_sha256(manifest: &AssistedRuntimeBundleManifest) -> Result<String> {
    let mut files = manifest
        .members
        .iter()
        .filter(|member| member.role == BundleMemberRole::FrontendAsset)
        .map(|member| {
            let path = member
                .path
                .strip_prefix("web/")
                .filter(|path| !path.is_empty())
                .ok_or_else(|| bundle_invalid("frontend member is outside the fixed web root"))?;
            Ok(FrontendAssetSnapshotMember {
                path: path.to_owned(),
                bytes: member.size,
                sha256: member.sha256.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    files.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let snapshot = FrontendAssetSnapshotManifest {
        schema_version: "migration-stream/frontend-asset-snapshot/v1",
        files,
    };
    Ok(sha256_hex(canonical_json_bytes(&serde_json::to_value(
        snapshot,
    )?)?))
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum BundleMemberRoleKey {
    PlatformLauncher,
    RuntimeController,
    IntegrityCli,
    Workbench,
    FrontendAsset,
    Policy,
    ProfileSchema,
    MappingSchema,
    BundleSchema,
    ControlledPack,
    OperatorGuide,
}

impl From<BundleMemberRole> for BundleMemberRoleKey {
    fn from(value: BundleMemberRole) -> Self {
        match value {
            BundleMemberRole::PlatformLauncher => Self::PlatformLauncher,
            BundleMemberRole::RuntimeController => Self::RuntimeController,
            BundleMemberRole::IntegrityCli => Self::IntegrityCli,
            BundleMemberRole::Workbench => Self::Workbench,
            BundleMemberRole::FrontendAsset => Self::FrontendAsset,
            BundleMemberRole::Policy => Self::Policy,
            BundleMemberRole::ProfileSchema => Self::ProfileSchema,
            BundleMemberRole::MappingSchema => Self::MappingSchema,
            BundleMemberRole::BundleSchema => Self::BundleSchema,
            BundleMemberRole::ControlledPack => Self::ControlledPack,
            BundleMemberRole::OperatorGuide => Self::OperatorGuide,
        }
    }
}

fn require_exact_role_path(
    manifest: &AssistedRuntimeBundleManifest,
    role: BundleMemberRole,
    path: &str,
    label: &str,
) -> Result<()> {
    let matching = manifest
        .members
        .iter()
        .filter(|member| member.role == role)
        .collect::<Vec<_>>();
    if matching.len() != 1 || matching[0].path != path {
        return Err(bundle_invalid(format!(
            "manifest does not bind exactly one fixed {label}"
        )));
    }
    Ok(())
}

fn calculate_manifest_self_hash(manifest: &AssistedRuntimeBundleManifest) -> Result<String> {
    let mut normalized = manifest.clone();
    ZERO_SHA256.clone_into(&mut normalized.manifest_sha256);
    Ok(sha256_hex(canonical_json_bytes(&serde_json::to_value(
        normalized,
    )?)?))
}

#[derive(Debug, Default)]
struct ActualBundleGraph {
    directories: BTreeSet<String>,
    files: BTreeSet<String>,
}

fn collect_actual_bundle_graph(
    root: &Path,
    maximum_file_count: usize,
    maximum_directory_count: usize,
) -> Result<ActualBundleGraph> {
    let mut graph = ActualBundleGraph::default();
    graph.directories.insert(String::new());
    let mut observed_file_count = 0_usize;
    collect_actual_directory(
        root,
        root,
        &mut graph,
        &mut observed_file_count,
        maximum_file_count,
        maximum_directory_count,
    )?;
    Ok(graph)
}

fn collect_actual_directory(
    root: &Path,
    directory: &Path,
    graph: &mut ActualBundleGraph,
    observed_file_count: &mut usize,
    maximum_file_count: usize,
    maximum_directory_count: usize,
) -> Result<()> {
    verify_directory_mode(directory)?;
    for entry in fs::read_dir(directory).map_err(|error| KernelError::io(directory, error))? {
        let entry = entry.map_err(|error| KernelError::io(directory, error))?;
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| KernelError::io(&path, error))?;
        if metadata.file_type().is_symlink() {
            return Err(bundle_invalid(format!(
                "bundle contains symbolic link {}",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| bundle_invalid("bundle path escaped its root"))?;
        let relative = path_to_manifest_string(relative)?;
        if metadata.is_dir() {
            graph.directories.insert(relative);
            if graph.directories.len() > maximum_directory_count {
                return Err(bundle_invalid(
                    "bundle directory graph exceeds its manifest-derived bound",
                ));
            }
            collect_actual_directory(
                root,
                &path,
                graph,
                observed_file_count,
                maximum_file_count,
                maximum_directory_count,
            )?;
        } else if metadata.is_file() {
            *observed_file_count = observed_file_count
                .checked_add(1)
                .ok_or_else(|| bundle_invalid("bundle file count overflow"))?;
            if *observed_file_count > maximum_file_count {
                return Err(bundle_invalid(
                    "bundle file graph exceeds its manifest-derived bound",
                ));
            }
            reject_hard_link(&path, &metadata)?;
            if relative != BUNDLE_MANIFEST_FILENAME {
                graph.files.insert(relative);
            }
        } else {
            return Err(bundle_invalid(format!(
                "bundle contains unsupported file type {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn expected_directory_set(manifest: &AssistedRuntimeBundleManifest) -> Result<BTreeSet<String>> {
    let mut directories = BTreeSet::from([String::new()]);
    for member in &manifest.members {
        let mut current = PathBuf::new();
        let path = Path::new(&member.path);
        if let Some(parent) = path.parent() {
            for component in parent.components() {
                let Component::Normal(value) = component else {
                    return Err(bundle_invalid("manifest member parent is unsafe"));
                };
                current.push(value);
                directories.insert(path_to_manifest_string(&current)?);
            }
        }
    }
    Ok(directories)
}

fn publish_archive_outputs(
    bundle_root: &Path,
    archive_path: &Path,
    checksum_path: &Path,
    verification: &BundleVerification,
) -> Result<(PublishedArchive, OwnedPathIdentity)> {
    let archive = write_deterministic_archive(bundle_root, archive_path, verification)?;
    let checksum = match write_archive_checksum(checksum_path, archive_path, &archive.sha256) {
        Ok(identity) => identity,
        Err(error) => {
            return Err(rollback_assembly_outputs(
                error,
                None,
                Some((archive_path, &archive.identity)),
                None,
            ));
        }
    };
    let verification_result = verify_archive_checksum(
        checksum_path,
        archive_path,
        &archive.sha256,
        &archive.identity,
    )
    .and_then(|()| {
        let replayed_archive_sha256 = verify_and_hash_deterministic_archive(
            archive_path,
            bundle_root,
            verification,
            &archive.identity,
        )?;
        if replayed_archive_sha256 != archive.sha256 {
            return Err(bundle_invalid(
                "archive identity changed after checksum publication",
            ));
        }
        let after = verify_bundle_root_for_target(bundle_root, &verification.target)?;
        if after.manifest_sha256 != verification.manifest_sha256 {
            return Err(bundle_invalid(
                "bundle identity changed after archive publication",
            ));
        }
        Ok(())
    });
    match verification_result {
        Ok(()) => Ok((archive, checksum)),
        Err(error) => Err(rollback_assembly_outputs(
            error,
            None,
            Some((archive_path, &archive.identity)),
            Some((checksum_path, &checksum)),
        )),
    }
}

// Archive publication keeps member ordering, ZIP metadata, durability, and
// post-write identity checks together so the signed-off byte stream is auditable.
#[allow(clippy::too_many_lines)]
fn write_deterministic_archive(
    bundle_root: &Path,
    archive_path: &Path,
    verification: &BundleVerification,
) -> Result<PublishedArchive> {
    let archive_parent = archive_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| bundle_invalid("bundle archive has no parent"))?;
    let archive_name = archive_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| safe_component(value))
        .ok_or_else(|| bundle_invalid("bundle archive name is unsafe"))?;
    let partial = archive_parent.join(format!(".{archive_name}.part-{}", Uuid::new_v4()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .map_err(|error| KernelError::io(&partial, error))?;
    let mut partial_guard = PartialFileGuard {
        path: partial.clone(),
        identity: capture_open_file_identity(&partial, &file)?,
        published: false,
    };
    set_mode(&partial, 0o644)?;
    let mut writer = ZipWriter::new(file);
    let root_name = bundle_root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| safe_component(value))
        .ok_or_else(|| bundle_invalid("bundle root name is unsafe"))?;
    let directory_options = deterministic_zip_options(0o555);
    let file_options = deterministic_zip_options(0o444);
    writer
        .add_directory(format!("{root_name}/"), directory_options)
        .map_err(|error| zip_error(&error))?;

    let manifest_bytes = read_regular_file_bounded(
        &bundle_root.join(BUNDLE_MANIFEST_FILENAME),
        MAXIMUM_MANIFEST_BYTES,
    )?;
    let manifest_entry = format!("{root_name}/{BUNDLE_MANIFEST_FILENAME}");
    writer
        .start_file(manifest_entry, file_options)
        .map_err(|error| zip_error(&error))?;
    writer
        .write_all(&manifest_bytes)
        .map_err(|error| zip_io_error(&error))?;

    let manifest = load_verified_manifest(bundle_root, verification)?;
    let directories = expected_directory_set(&manifest)?;
    for directory in directories.iter().filter(|path| !path.is_empty()) {
        writer
            .add_directory(format!("{root_name}/{directory}/"), directory_options)
            .map_err(|error| zip_error(&error))?;
    }
    for member in &manifest.members {
        let options = deterministic_zip_options(parse_mode(&member.mode)?);
        writer
            .start_file(format!("{root_name}/{}", member.path), options)
            .map_err(|error| zip_error(&error))?;
        let bytes =
            read_regular_file_bounded(&bundle_root.join(&member.path), MAXIMUM_MEMBER_BYTES)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != member.size
            || sha256_hex(&bytes) != member.sha256
        {
            return Err(bundle_invalid(format!(
                "bundle member {} changed while the archive was written",
                member.path
            )));
        }
        writer
            .write_all(&bytes)
            .map_err(|error| zip_io_error(&error))?;
    }
    let finished = writer.finish().map_err(|error| zip_error(&error))?;
    finished
        .sync_all()
        .map_err(|error| KernelError::io(&partial, error))?;
    let identity = capture_open_file_identity(&partial, &finished)?;
    rename_noreplace(&partial, archive_path)?;
    partial_guard.published = true;
    let publication_result =
        verify_named_open_identity(archive_path, &finished, &identity.metadata)
            .and_then(|()| sync_directory(archive_parent))
            .and_then(|()| {
                verify_and_hash_deterministic_archive(
                    archive_path,
                    bundle_root,
                    verification,
                    &identity,
                )
            });
    // Close the publishing handle before exact rollback opens a zero-share
    // DELETE handle. The identity above was captured from this same handle.
    drop(finished);
    match publication_result {
        Ok(sha256) => Ok(PublishedArchive { sha256, identity }),
        Err(error) => Err(rollback_assembly_outputs(
            error,
            None,
            Some((archive_path, &identity)),
            None,
        )),
    }
}

fn verify_and_hash_deterministic_archive(
    archive_path: &Path,
    bundle_root: &Path,
    verification: &BundleVerification,
    expected_identity: &OwnedPathIdentity,
) -> Result<String> {
    let (file, opened_metadata) = open_retained_regular_file(archive_path)?;
    if !same_owned_file_identity(archive_path, &file, &opened_metadata, expected_identity)? {
        return Err(bundle_invalid(
            "bundle archive identity changed before semantic replay",
        ));
    }
    if opened_metadata.len() > MAXIMUM_BUNDLE_BYTES {
        return Err(bundle_invalid("bundle archive exceeds its byte bound"));
    }
    let mut archive = ZipArchive::new(file).map_err(|error| zip_error(&error))?;
    let manifest = load_verified_manifest(bundle_root, verification)?;
    let root_name = bundle_root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| safe_component(value))
        .ok_or_else(|| bundle_invalid("bundle root name is unsafe"))?;
    let mut expected = vec![(format!("{root_name}/"), true, 0o555, None::<(u64, String)>)];
    let manifest_bytes = read_regular_file_bounded(
        &bundle_root.join(BUNDLE_MANIFEST_FILENAME),
        MAXIMUM_MANIFEST_BYTES,
    )?;
    expected.push((
        format!("{root_name}/{BUNDLE_MANIFEST_FILENAME}"),
        false,
        0o444,
        Some((
            u64::try_from(manifest_bytes.len())
                .map_err(|_| bundle_invalid("manifest byte size exceeds u64"))?,
            sha256_hex(&manifest_bytes),
        )),
    ));
    for directory in expected_directory_set(&manifest)?
        .into_iter()
        .filter(|path| !path.is_empty())
    {
        expected.push((format!("{root_name}/{directory}/"), true, 0o555, None));
    }
    for member in &manifest.members {
        expected.push((
            format!("{root_name}/{}", member.path),
            false,
            parse_mode(&member.mode)?,
            Some((member.size, member.sha256.clone())),
        ));
    }
    if archive.len() != expected.len() {
        return Err(bundle_invalid(
            "bundle archive member count differs from the manifest graph",
        ));
    }
    for (index, (expected_name, expected_directory, expected_mode, expected_content)) in
        expected.into_iter().enumerate()
    {
        let mut entry = archive.by_index(index).map_err(|error| zip_error(&error))?;
        if entry.name().map_err(|error| zip_error(&error))? != expected_name
            || entry.is_dir() != expected_directory
            || entry.unix_mode().map(|mode| mode & 0o777) != Some(expected_mode)
            || entry.enclosed_name().is_none()
        {
            return Err(bundle_invalid(
                "bundle archive entry identity, type, or mode differs from the verified graph",
            ));
        }
        if let Some((expected_size, expected_hash)) = expected_content {
            if entry.size() != expected_size {
                return Err(bundle_invalid(
                    "bundle archive entry size differs from the manifest",
                ));
            }
            let mut bytes = Vec::new();
            (&mut entry)
                .take(MAXIMUM_MEMBER_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| zip_io_error(&error))?;
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != expected_size
                || sha256_hex(&bytes) != expected_hash
            {
                return Err(bundle_invalid(
                    "bundle archive entry bytes differ from the manifest",
                ));
            }
        } else if entry.size() != 0 {
            return Err(bundle_invalid(
                "bundle archive directory entry is not empty",
            ));
        }
    }
    let mut file = archive.into_inner();
    file.seek(SeekFrom::Start(0))
        .map_err(|error| KernelError::io(archive_path, error))?;
    let (observed, sha256) = hash_open_file_bounded(&mut file, archive_path, MAXIMUM_BUNDLE_BYTES)?;
    if observed != opened_metadata.len() {
        return Err(bundle_invalid("bundle archive changed while hashing"));
    }
    verify_named_open_identity(archive_path, &file, &opened_metadata)?;
    Ok(sha256)
}

fn load_verified_manifest(
    bundle_root: &Path,
    verification: &BundleVerification,
) -> Result<AssistedRuntimeBundleManifest> {
    let bytes = read_regular_file_bounded(
        &bundle_root.join(BUNDLE_MANIFEST_FILENAME),
        MAXIMUM_MANIFEST_BYTES,
    )?;
    let value = parse_unique_json(&bytes)
        .map_err(|error| bundle_invalid(format!("manifest JSON is invalid: {error}")))?;
    let manifest: AssistedRuntimeBundleManifest = serde_json::from_value(value)?;
    if canonical_json_bytes(&serde_json::to_value(&manifest)?)? != bytes {
        return Err(bundle_invalid(
            "verified manifest became noncanonical before archive creation",
        ));
    }
    validate_manifest(&manifest, &verification.target)?;
    if manifest.manifest_sha256 != verification.manifest_sha256 {
        return Err(bundle_invalid(
            "verified manifest changed before archive creation",
        ));
    }
    Ok(manifest)
}

fn deterministic_zip_options(mode: u32) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .system(System::Unix)
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(6))
        .last_modified_time(DateTime::default())
        .unix_permissions(mode)
}

fn validate_product_commit(value: &str) -> Result<()> {
    if value.len() != 40
        || value.len() > MAXIMUM_PRODUCT_COMMIT_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(bundle_invalid(
            "product commit must be one lowercase 40-character Git object ID",
        ));
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(bundle_invalid(format!("{label} must be lowercase SHA-256")));
    }
    Ok(())
}

fn validate_member_path(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > MAXIMUM_PATH_BYTES
        || !value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || value.contains('\\')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
    {
        return Err(bundle_invalid(format!(
            "bundle member path {value:?} is unsafe"
        )));
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            let Component::Normal(value) = component else {
                return true;
            };
            value.to_str().is_none_or(|value| !safe_component(value))
        })
    {
        return Err(bundle_invalid(format!(
            "bundle member path {value:?} is not normalized"
        )));
    }
    Ok(())
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value != "."
        && value != ".."
        && value.len() <= MAXIMUM_PATH_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn verify_platform_executable(path: &Path, target: &BundleTarget) -> Result<()> {
    match (target.os.as_str(), target.architecture.as_str()) {
        ("macos", "arm64") => verify_arm64_macho_executable(path),
        ("linux", "x64") => verify_elf_executable(path, 62),
        ("linux", "arm64") => verify_elf_executable(path, 183),
        ("windows", "x64") => verify_pe_executable(path, 0x8664),
        ("windows", "arm64") => verify_pe_executable(path, 0xaa64),
        _ => Err(bundle_invalid(
            "bundle target lacks a supported executable contract",
        )),
    }
}

fn verify_arm64_macho_executable(path: &Path) -> Result<()> {
    let (mut file, metadata) = open_retained_regular_file(path)?;
    if metadata.len() < 16 {
        return Err(bundle_invalid(format!(
            "bundle executable {} is not a thin arm64 Mach-O",
            path.display()
        )));
    }
    let mut header = [0_u8; 16];
    file.read_exact(&mut header)
        .map_err(|error| KernelError::io(path, error))?;
    verify_named_open_identity(path, &file, &metadata)?;
    if header[..4] != [0xcf, 0xfa, 0xed, 0xfe]
        || header[4..8] != [0x0c, 0x00, 0x00, 0x01]
        || header[12..16] != [0x02, 0x00, 0x00, 0x00]
    {
        return Err(bundle_invalid(format!(
            "bundle executable {} is not a thin arm64 Mach-O executable",
            path.display()
        )));
    }
    Ok(())
}

fn verify_elf_executable(path: &Path, expected_machine: u16) -> Result<()> {
    let (mut file, metadata) = open_retained_regular_file(path)?;
    if metadata.len() < 20 {
        return Err(bundle_invalid(format!(
            "bundle executable {} is not one ELF64 binary",
            path.display()
        )));
    }
    let mut header = [0_u8; 20];
    file.read_exact(&mut header)
        .map_err(|error| KernelError::io(path, error))?;
    verify_named_open_identity(path, &file, &metadata)?;
    let machine = u16::from_le_bytes([header[18], header[19]]);
    if header[..7] != [0x7f, b'E', b'L', b'F', 2, 1, 1] || machine != expected_machine {
        return Err(bundle_invalid(format!(
            "bundle executable {} is not the expected ELF64 target",
            path.display()
        )));
    }
    Ok(())
}

fn verify_pe_executable(path: &Path, expected_machine: u16) -> Result<()> {
    let (mut file, metadata) = open_retained_regular_file(path)?;
    if metadata.len() < 256 {
        return Err(bundle_invalid(format!(
            "bundle executable {} is not one PE64 binary",
            path.display()
        )));
    }
    let mut first = [0_u8; 64];
    file.read_exact(&mut first)
        .map_err(|error| KernelError::io(path, error))?;
    let offset = usize::try_from(u32::from_le_bytes([
        first[60], first[61], first[62], first[63],
    ]))
    .map_err(|_| bundle_invalid("PE executable header offset exceeds host bounds"))?;
    if offset > 1024 * 1024
        || offset.saturating_add(26) > usize::try_from(metadata.len()).unwrap_or(usize::MAX)
    {
        return Err(bundle_invalid(
            "PE executable header offset exceeds its bound",
        ));
    }
    file.seek(SeekFrom::Start(
        u64::try_from(offset).map_err(|_| bundle_invalid("PE offset overflow"))?,
    ))
    .map_err(|error| KernelError::io(path, error))?;
    let mut header = [0_u8; 26];
    file.read_exact(&mut header)
        .map_err(|error| KernelError::io(path, error))?;
    verify_named_open_identity(path, &file, &metadata)?;
    let machine = u16::from_le_bytes([header[4], header[5]]);
    let optional_magic = u16::from_le_bytes([header[24], header[25]]);
    if first[..2] != *b"MZ"
        || header[..4] != *b"PE\0\0"
        || machine != expected_machine
        || optional_magic != 0x20b
    {
        return Err(bundle_invalid(format!(
            "bundle executable {} is not the expected PE64 target",
            path.display()
        )));
    }
    Ok(())
}

fn path_to_manifest_string(path: &Path) -> Result<String> {
    let mut values = Vec::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(bundle_invalid("bundle path is not normalized"));
        };
        let value = value
            .to_str()
            .filter(|value| safe_component(value))
            .ok_or_else(|| bundle_invalid("bundle path component is unsafe"))?;
        values.push(value);
    }
    let result = values.join("/");
    validate_member_path(&result)?;
    Ok(result)
}

fn create_directory_tree(root: &Path, destination: &Path) -> Result<()> {
    let relative = destination
        .strip_prefix(root)
        .map_err(|_| bundle_invalid("bundle destination escaped its root"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(value) = component else {
            return Err(bundle_invalid("bundle destination is not normalized"));
        };
        current.push(value);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(bundle_invalid(format!(
                        "bundle destination {} is not a real directory",
                        current.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_directory(&current)?;
            }
            Err(error) => return Err(KernelError::io(&current, error)),
        }
        verify_assembly_directory(&current)?;
    }
    Ok(())
}

fn create_directory(path: &Path) -> Result<()> {
    fs::create_dir(path).map_err(|error| KernelError::io(path, error))?;
    set_mode(path, 0o755)?;
    sync_directory(
        path.parent()
            .ok_or_else(|| bundle_invalid("new directory has no parent"))?,
    )
}

fn create_owned_directory(path: &Path) -> Result<OwnedPathIdentity> {
    fs::create_dir(path).map_err(|error| KernelError::io(path, error))?;
    let identity = capture_owned_directory(path)?;
    if let Err(error) = set_mode(path, 0o755).and_then(|()| {
        sync_directory(
            path.parent()
                .ok_or_else(|| bundle_invalid("new directory has no parent"))?,
        )
    }) {
        return Err(rollback_assembly_outputs(
            error,
            Some((path, &identity)),
            None,
            None,
        ));
    }
    Ok(identity)
}

fn write_new_file(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| KernelError::io(path, error))?;
    set_mode(path, mode)?;
    file.write_all(bytes)
        .map_err(|error| KernelError::io(path, error))?;
    file.sync_all()
        .map_err(|error| KernelError::io(path, error))?;
    sync_directory(
        path.parent()
            .ok_or_else(|| bundle_invalid("new file has no parent"))?,
    )
}

fn require_real_directory(path: &Path, label: &str) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(bundle_invalid(format!(
            "{label} must be one real non-link directory"
        )));
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| KernelError::io(path, error))?;
    let canonical_metadata =
        fs::symlink_metadata(&canonical).map_err(|error| KernelError::io(&canonical, error))?;
    if !canonical_metadata.is_dir() || canonical_metadata.file_type().is_symlink() {
        return Err(bundle_invalid(format!(
            "{label} canonical identity is not one real directory"
        )));
    }
    Ok(canonical)
}

fn require_private_staging_directory(
    path: &Path,
    repository_root: &Path,
    label: &str,
) -> Result<PathBuf> {
    let directory = require_real_directory(path, label)?;
    if directory.starts_with(repository_root) {
        return Err(bundle_invalid(format!(
            "{label} must be staged in one private root outside the repository"
        )));
    }
    Ok(directory)
}

fn read_regular_file_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let (mut file, metadata) = open_retained_regular_file(path)?;
    if metadata.len() > maximum {
        return Err(bundle_invalid(format!(
            "file {} exceeds its byte bound",
            path.display()
        )));
    }
    let capacity = usize::try_from(metadata.len())
        .map_err(|_| bundle_invalid("file size exceeds platform memory bounds"))?;
    let mut bytes = Vec::with_capacity(capacity);
    (&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| KernelError::io(path, error))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != metadata.len() {
        return Err(bundle_invalid(format!(
            "file {} changed while reading",
            path.display()
        )));
    }
    verify_named_open_identity(path, &file, &metadata)?;
    Ok(bytes)
}

fn hash_regular_file_bounded(path: &Path, maximum: u64) -> Result<(u64, String)> {
    let (mut file, metadata) = open_retained_regular_file(path)?;
    if metadata.len() > maximum {
        return Err(bundle_invalid(format!(
            "file {} exceeds its byte bound",
            path.display()
        )));
    }
    let (observed, sha256) = hash_open_file_bounded(&mut file, path, maximum)?;
    if observed != metadata.len() {
        return Err(bundle_invalid(format!(
            "file {} changed while hashing",
            path.display()
        )));
    }
    verify_named_open_identity(path, &file, &metadata)?;
    Ok((observed, sha256))
}

fn hash_exact_owned_file_bounded(
    path: &Path,
    maximum: u64,
    expected_identity: &OwnedPathIdentity,
) -> Result<(u64, String)> {
    let (mut file, metadata) = open_retained_regular_file(path)?;
    if !same_owned_file_identity(path, &file, &metadata, expected_identity)? {
        return Err(bundle_invalid(
            "published output identity changed before hashing",
        ));
    }
    if metadata.len() > maximum {
        return Err(bundle_invalid(format!(
            "file {} exceeds its byte bound",
            path.display()
        )));
    }
    let (observed, sha256) = hash_open_file_bounded(&mut file, path, maximum)?;
    if observed != metadata.len() {
        return Err(bundle_invalid(format!(
            "file {} changed while hashing",
            path.display()
        )));
    }
    verify_named_open_identity(path, &file, &metadata)?;
    Ok((observed, sha256))
}

fn hash_open_file_bounded(file: &mut File, path: &Path, maximum: u64) -> Result<(u64, String)> {
    let mut digest = Sha256::new();
    let mut observed = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| KernelError::io(path, error))?;
        if read == 0 {
            break;
        }
        observed = observed
            .checked_add(
                u64::try_from(read).map_err(|_| bundle_invalid("file read size exceeds u64"))?,
            )
            .ok_or_else(|| bundle_invalid("file read size overflow"))?;
        if observed > maximum {
            return Err(bundle_invalid(format!(
                "file {} exceeds its byte bound",
                path.display()
            )));
        }
        digest.update(&buffer[..read]);
    }
    Ok((observed, crate::canonical::lower_hex(digest.finalize())))
}

fn open_retained_regular_file(path: &Path) -> Result<(File, fs::Metadata)> {
    let before = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(bundle_invalid(format!(
            "{} must be one real regular file",
            path.display()
        )));
    }
    reject_hard_link(path, &before)?;
    let file = File::open(path).map_err(|error| KernelError::io(path, error))?;
    let opened = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    if !same_file_identity(&before, &opened) {
        return Err(bundle_invalid(format!(
            "{} changed while opening",
            path.display()
        )));
    }
    verify_named_open_identity(path, &file, &opened)?;
    Ok((file, opened))
}

fn verify_named_open_identity(path: &Path, file: &File, opened: &fs::Metadata) -> Result<()> {
    let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    #[cfg(not(windows))]
    let current = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    if !named.is_file() || named.file_type().is_symlink() {
        return Err(bundle_invalid(format!(
            "{} identity changed during verification",
            path.display()
        )));
    }
    #[cfg(windows)]
    {
        let _ = opened;
        let named_identity = migration_stream_windows_native::RealFile::capture(path)
            .map_err(|error| KernelError::io(path, error))?
            .identity();
        let opened_identity = migration_stream_windows_native::file_identity_from_open_file(file)
            .map_err(|error| KernelError::io(path, error))?;
        if named_identity != opened_identity {
            return Err(bundle_invalid(format!(
                "{} identity changed during verification",
                path.display()
            )));
        }
    }
    #[cfg(not(windows))]
    if !same_file_identity(opened, &current) || !same_file_identity(&current, &named) {
        return Err(bundle_invalid(format!(
            "{} identity changed during verification",
            path.display()
        )));
    }
    reject_hard_link(path, &named)
}

// System-provided executable files may legitimately have package-manager
// hard-link aliases. Git is never copied into a bundle, so its authority is
// exact named/open identity, not the bundle-member single-link invariant.
fn verify_named_open_identity_allow_hard_links(
    path: &Path,
    file: &File,
    opened: &fs::Metadata,
) -> Result<()> {
    let named = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    #[cfg(not(windows))]
    let current = file
        .metadata()
        .map_err(|error| KernelError::io(path, error))?;
    if !named.is_file() || named.file_type().is_symlink() {
        return Err(bundle_invalid(format!(
            "{} identity changed during verification",
            path.display()
        )));
    }
    #[cfg(windows)]
    {
        let _ = opened;
        let named_identity = migration_stream_windows_native::RealFile::capture(path)
            .map_err(|error| KernelError::io(path, error))?
            .identity();
        let opened_identity = migration_stream_windows_native::file_identity_from_open_file(file)
            .map_err(|error| KernelError::io(path, error))?;
        if named_identity != opened_identity {
            return Err(bundle_invalid(format!(
                "{} identity changed during verification",
                path.display()
            )));
        }
    }
    #[cfg(not(windows))]
    if !same_file_identity(opened, &current) || !same_file_identity(&current, &named) {
        return Err(bundle_invalid(format!(
            "{} identity changed during verification",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len()
}

#[cfg(unix)]
fn reject_hard_link(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    if metadata.nlink() != 1 {
        return Err(bundle_invalid(format!(
            "bundle file {} has a hard-link alias",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
// Keep the verification API identical across platforms; Windows has no Unix
// hard-link count to validate for this bundle contract.
#[allow(clippy::unnecessary_wraps)]
fn reject_hard_link(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| KernelError::io(path, error))
}

#[cfg(not(unix))]
// Keep the assembly API fallible and platform-neutral although Windows does
// not apply Unix permission bits.
#[allow(clippy::unnecessary_wraps)]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn verify_file_mode(path: &Path, expected: &str) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    let observed = metadata.mode() & 0o777;
    if observed != parse_mode(expected)? {
        return Err(bundle_invalid(format!(
            "bundle file {} mode is {observed:04o}, expected {expected}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
// Keep the verifier API identical across platforms; mode authority is Unix
// specific and is deliberately not approximated on Windows.
#[allow(clippy::unnecessary_wraps)]
fn verify_file_mode(_path: &Path, _expected: &str) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn verify_directory_mode(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.mode() & 0o777 != 0o555 {
        return Err(bundle_invalid(format!(
            "bundle directory {} must be one mode-0555 real directory",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_directory_mode(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(bundle_invalid(format!(
            "bundle directory {} must be one real directory",
            path.display()
        )));
    }
    Ok(())
}

fn parse_mode(value: &str) -> Result<u32> {
    u32::from_str_radix(value, 8).map_err(|_| bundle_invalid("bundle mode is invalid"))
}

#[cfg(unix)]
fn verify_assembly_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.mode() & 0o777 != 0o755 {
        return Err(bundle_invalid(format!(
            "bundle assembly directory {} must be one mode-0755 real directory",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_assembly_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(bundle_invalid(format!(
            "bundle assembly directory {} must be one real directory",
            path.display()
        )));
    }
    Ok(())
}

fn seal_bundle_directories(root: &Path) -> Result<()> {
    let mut directories = vec![root.to_path_buf()];
    let mut index = 0;
    while index < directories.len() {
        let directory = directories[index].clone();
        index += 1;
        for entry in fs::read_dir(&directory).map_err(|error| KernelError::io(&directory, error))? {
            let entry = entry.map_err(|error| KernelError::io(&directory, error))?;
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|error| KernelError::io(&path, error))?;
            if metadata.file_type().is_symlink() {
                return Err(bundle_invalid("bundle assembly produced a link"));
            }
            if metadata.is_dir() {
                directories.push(path);
            } else if !metadata.is_file() {
                return Err(bundle_invalid(
                    "bundle assembly produced an unsupported file type",
                ));
            }
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        set_mode(&directory, 0o555)?;
    }
    Ok(())
}

#[cfg(any(not(windows), test))]
fn remove_owned_bundle_tree(root: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(KernelError::io(root, error)),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(bundle_invalid(
            "bundle rollback target is not one real directory",
        ));
    }
    let mut directories = vec![root.to_path_buf()];
    let mut index = 0;
    let mut observed_entries = 0_usize;
    while index < directories.len() {
        let directory = directories[index].clone();
        index += 1;
        for entry in fs::read_dir(&directory).map_err(|error| KernelError::io(&directory, error))? {
            let path = entry
                .map_err(|error| KernelError::io(&directory, error))?
                .path();
            observed_entries = observed_entries
                .checked_add(1)
                .ok_or_else(|| bundle_invalid("bundle rollback entry count overflow"))?;
            if observed_entries > MAXIMUM_ROLLBACK_ENTRY_COUNT {
                return Err(bundle_invalid(
                    "bundle rollback tree exceeds its explicit entry bound",
                ));
            }
            let metadata =
                fs::symlink_metadata(&path).map_err(|error| KernelError::io(&path, error))?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                directories.push(path);
            }
        }
    }
    for directory in &directories {
        set_mode(directory, 0o700)?;
    }
    let parent = root
        .parent()
        .ok_or_else(|| bundle_invalid("bundle rollback target has no parent"))?;
    fs::remove_dir_all(root).map_err(|error| KernelError::io(root, error))?;
    sync_directory(parent)
}

fn remove_exact_owned_bundle_tree(root: &Path, identity: &OwnedPathIdentity) -> Result<()> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(KernelError::io(root, error)),
    };
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || !same_owned_directory_identity(root, &metadata, identity)?
    {
        return Err(bundle_invalid(
            "bundle rollback refused a replaced directory target",
        ));
    }
    #[cfg(windows)]
    let removal = {
        // The complete root FileId was replayed above, but std cannot remove a
        // directory tree through that retained handle. A path-recursive delete
        // could cross a later namespace replacement or foreign injection, so
        // Windows deliberately preserves this partial for explicit recovery.
        Err(bundle_invalid(
            "Windows bundle-directory rollback preserves an exact but non-recursively-deletable partial",
        ))
    };
    #[cfg(not(windows))]
    let removal = remove_owned_bundle_tree(root);
    removal
}

fn remove_exact_owned_file(path: &Path, identity: &OwnedPathIdentity) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(KernelError::io(path, error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(bundle_invalid(
            "bundle rollback refused a replaced regular-file target",
        ));
    }
    let file = File::open(path).map_err(|error| KernelError::io(path, error))?;
    if !same_owned_file_identity(path, &file, &metadata, identity)? {
        return Err(bundle_invalid(
            "bundle rollback refused a replaced regular-file target",
        ));
    }
    drop(file);
    #[cfg(windows)]
    {
        migration_stream_windows_native::delete_file_exact(path, identity.native_identity)
            .map_err(|error| KernelError::io(path, error))?;
        return sync_directory(
            path.parent()
                .ok_or_else(|| bundle_invalid("bundle rollback file has no parent"))?,
        );
    }
    #[cfg(not(windows))]
    fs::remove_file(path).map_err(|error| KernelError::io(path, error))?;
    #[cfg(not(windows))]
    sync_directory(
        path.parent()
            .ok_or_else(|| bundle_invalid("bundle rollback file has no parent"))?,
    )
}

fn rollback_assembly_outputs(
    primary: KernelError,
    directory: Option<(&Path, &OwnedPathIdentity)>,
    archive: Option<(&Path, &OwnedPathIdentity)>,
    checksum: Option<(&Path, &OwnedPathIdentity)>,
) -> KernelError {
    let mut failures = Vec::new();
    if let Some((path, identity)) = checksum
        && let Err(error) = remove_exact_owned_file(path, identity)
    {
        failures.push(error.to_string());
    }
    if let Some((path, identity)) = archive
        && let Err(error) = remove_exact_owned_file(path, identity)
    {
        failures.push(error.to_string());
    }
    if let Some((path, identity)) = directory
        && let Err(error) = remove_exact_owned_bundle_tree(path, identity)
    {
        failures.push(error.to_string());
    }
    if failures.is_empty() {
        primary
    } else {
        bundle_invalid(format!(
            "{primary}; rollback also failed: {}",
            failures.join("; ")
        ))
    }
}

fn path_lexically_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(KernelError::io(path, error)),
    }
}

fn write_archive_checksum(
    checksum_path: &Path,
    archive_path: &Path,
    archive_sha256: &str,
) -> Result<OwnedPathIdentity> {
    validate_sha256(archive_sha256, "archive SHA-256")?;
    let archive_name = archive_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| safe_component(value))
        .ok_or_else(|| bundle_invalid("bundle archive name is unsafe"))?;
    let bytes = format!("{archive_sha256}  {archive_name}\n").into_bytes();
    let parent = checksum_path
        .parent()
        .ok_or_else(|| bundle_invalid("archive checksum has no parent"))?;
    let checksum_name = checksum_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| safe_component(value))
        .ok_or_else(|| bundle_invalid("archive checksum name is unsafe"))?;
    let partial = parent.join(format!(".{checksum_name}.part-{}", Uuid::new_v4()));
    // The guard deliberately outlives the write handle. Its Windows exact
    // deletion uses a zero-share DELETE handle, so cleanup must run only after
    // the writer has closed.
    let mut partial_guard = None;
    let mut published_identity = None;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .map_err(|error| KernelError::io(&partial, error))?;
        let identity = capture_open_file_identity(&partial, &file)?;
        partial_guard = Some(PartialFileGuard {
            path: partial.clone(),
            identity,
            published: false,
        });
        file.write_all(&bytes)
            .map_err(|error| KernelError::io(&partial, error))?;
        file.sync_all()
            .map_err(|error| KernelError::io(&partial, error))?;
        set_mode(&partial, 0o444)?;
        let identity = capture_open_file_identity(&partial, &file)?;
        rename_noreplace(&partial, checksum_path)?;
        partial_guard
            .as_mut()
            .expect("checksum partial guard is established before publication")
            .published = true;
        published_identity = Some(identity.clone());
        verify_named_open_identity(checksum_path, &file, &identity.metadata)
            .and_then(|()| sync_directory(parent))?;
        Ok(identity)
    })();
    // The closure owns and closes `file` before any partial guard or published
    // checksum rollback is dropped or invoked.
    drop(partial_guard);
    match result {
        Ok(identity) => Ok(identity),
        Err(error) => match published_identity.as_ref() {
            Some(identity) => Err(rollback_assembly_outputs(
                error,
                None,
                None,
                Some((checksum_path, identity)),
            )),
            None => Err(error),
        },
    }
}

fn verify_archive_checksum(
    checksum_path: &Path,
    archive_path: &Path,
    archive_sha256: &str,
    archive_identity: &OwnedPathIdentity,
) -> Result<()> {
    verify_file_mode(checksum_path, "0444")?;
    let archive_name = archive_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| safe_component(value))
        .ok_or_else(|| bundle_invalid("bundle archive name is unsafe"))?;
    let expected = format!("{archive_sha256}  {archive_name}\n").into_bytes();
    let observed = read_regular_file_bounded(checksum_path, 1024)?;
    if observed != expected {
        return Err(bundle_invalid(
            "archive checksum sidecar differs from the exact archive identity",
        ));
    }
    let (_, observed_archive_sha256) =
        hash_exact_owned_file_bounded(archive_path, MAXIMUM_BUNDLE_BYTES, archive_identity)?;
    if observed_archive_sha256 != archive_sha256 {
        return Err(bundle_invalid(
            "published archive differs from its checksum sidecar",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| KernelError::io(path, error))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_noreplace(source: &Path, destination: &Path) -> Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};

    renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE)
        .map_err(|error| KernelError::io(destination, std::io::Error::from(error)))
}

#[cfg(windows)]
fn rename_noreplace(source: &Path, destination: &Path) -> Result<()> {
    // `move_atomic` issues `MoveFileExW` without `REPLACE_EXISTING`, so the
    // namespace transition—not a racy existence pre-check—enforces no-replace.
    crate::durability::publish_new(source, destination)
        .map_err(|error| KernelError::io(destination, error))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn rename_noreplace(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() {
        return Err(bundle_invalid("no-replace destination already exists"));
    }
    fs::rename(source, destination).map_err(|error| KernelError::io(destination, error))
}

#[cfg(not(unix))]
// Directory fsync has no Windows equivalent in this portability layer, but
// callers must retain the same failure-aware protocol shape.
#[allow(clippy::unnecessary_wraps)]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn zip_error(error: &zip::result::ZipError) -> KernelError {
    bundle_invalid(format!("bundle ZIP creation failed: {error}"))
}

fn zip_io_error(error: &std::io::Error) -> KernelError {
    bundle_invalid(format!("bundle ZIP write failed: {error}"))
}

fn bundle_invalid(message: impl Into<String>) -> KernelError {
    KernelError::BundleInvalid(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestBundle {
        directory: PathBuf,
        parent: tempfile::TempDir,
    }

    impl Drop for TestBundle {
        fn drop(&mut self) {
            let _ = remove_owned_bundle_tree(&self.directory);
        }
    }

    #[test]
    fn manifest_self_hash_changes_when_authority_changes() {
        let mut manifest = minimal_manifest();
        manifest.manifest_sha256 =
            calculate_manifest_self_hash(&manifest).expect("manifest self hash");
        assert_eq!(
            calculate_manifest_self_hash(&manifest).expect("replay self hash"),
            manifest.manifest_sha256
        );
        manifest.product.commit = "b".repeat(40);
        assert_ne!(
            calculate_manifest_self_hash(&manifest).expect("changed self hash"),
            manifest.manifest_sha256
        );
    }

    #[test]
    fn unsafe_member_paths_fail_closed() {
        for value in [
            "",
            "/absolute",
            "../escape",
            "web/../escape",
            "web\\escape",
            ".DS_Store",
            "web/.hidden",
            "web/space name",
        ] {
            assert!(validate_member_path(value).is_err(), "{value:?} must fail");
        }
        validate_member_path("object-packs/synthetic.customer/1.0.0/manifest.json")
            .expect("safe member path");
    }

    #[test]
    fn release_binary_stage_must_be_a_private_real_directory() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let repository = temporary.path().join("repository");
        let private_stage = temporary.path().join("private-stage");
        fs::create_dir(&repository).expect("repository root");
        fs::create_dir(&private_stage).expect("private staging root");
        let repository = repository
            .canonicalize()
            .expect("canonical repository root");

        let staged = require_private_staging_directory(
            &private_stage,
            &repository,
            "release binary staging",
        )
        .expect("private staging root");
        assert_eq!(
            staged,
            private_stage
                .canonicalize()
                .expect("canonical private staging root")
        );
        assert!(
            require_private_staging_directory(&repository, &repository, "release binary staging")
                .is_err(),
            "the repository is never an admissible binary staging root"
        );
        let nested = repository.join("target/release-binaries");
        fs::create_dir_all(&nested).expect("nested repository staging root");
        assert!(
            require_private_staging_directory(&nested, &repository, "release binary staging")
                .is_err(),
            "an ignored target path is still not a private staging root"
        );
    }

    #[test]
    fn bundle_output_paths_use_one_canonical_parent_spelling() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let alias_component = temporary.path().join("alias-component");
        fs::create_dir(&alias_component).expect("alias component");
        let requested_parent = alias_component.join("..");
        let requested_output = requested_parent.join("migration-stream-local-windows-x64");
        let canonical_parent = require_real_directory(&requested_parent, "test output parent")
            .expect("canonical output parent");
        assert_ne!(
            requested_parent, canonical_parent,
            "fixture must exercise two spellings of the same parent"
        );

        let output_name = requested_output
            .file_name()
            .and_then(|value| value.to_str())
            .expect("safe output name");
        let (bundle_directory, archive_path, checksum_path) =
            canonical_bundle_output_paths(&canonical_parent, output_name);

        for path in [&bundle_directory, &archive_path, &checksum_path] {
            assert_eq!(path.parent(), Some(canonical_parent.as_path()));
        }
        assert_eq!(bundle_directory.file_name(), requested_output.file_name());
        assert_eq!(
            archive_path.file_name(),
            requested_output.with_extension("zip").file_name()
        );
        assert_eq!(
            checksum_path.file_name(),
            requested_output.with_extension("zip.sha256").file_name()
        );
    }

    #[test]
    fn platform_launchers_are_direct_and_target_exact() {
        assert!(PLATFORM_LAUNCHER.contains(
            "exec /usr/bin/env -i \"$bundle_root/libexec/migration-stream-local-runtime\" \"$@\""
        ));
        assert!(
            !PLATFORM_LAUNCHER
                .contains("exec \"$bundle_root/libexec/migration-stream-local-runtime\" \"$@\"")
        );
        assert_eq!(WINDOWS_LAUNCHER_PATH, "migration-stream-local.exe");
        assert!(
            platform_launcher_for(&target_for("windows", "x64").expect("Windows x64")).is_err()
        );
    }

    #[test]
    fn wrong_or_future_target_fails_closed() {
        let mut manifest = minimal_manifest();
        rehash(&mut manifest);
        let wrong = BundleTarget {
            os: "macos".to_owned(),
            architecture: "future-arch".to_owned(),
            target_triple: "future-apple-darwin".to_owned(),
            minimum_os_version: Some(BUNDLED_BINARY_MINIMUM_MACOS_VERSION.to_owned()),
        };
        let error = validate_manifest(&manifest, &wrong).expect_err("wrong target");
        assert_eq!(error.code(), "BUNDLE_PLATFORM_MISMATCH");
    }

    #[test]
    fn macos_host_policy_preserves_26_and_limits_27_to_the_exact_arm64_candidate() {
        for version in ["26", "26.0", "26.5.2", "26.99.1"] {
            assert_eq!(
                qualify_macos_host(version, "aarch64").unwrap(),
                MacosHostQualification::Existing26
            );
        }
        assert_eq!(
            qualify_macos_host("27.0.1", "aarch64").unwrap(),
            MacosHostQualification::Provisional27_0_1
        );
        for version in [
            "25.9", "27", "27.0", "27.0.2", "27.1", "28.0.1", "127.0.1", "",
        ] {
            assert_eq!(
                qualify_macos_host(version, "aarch64").unwrap_err().code(),
                "BUNDLE_PLATFORM_MISMATCH"
            );
        }
        for architecture in ["x86_64", "arm", "universal", ""] {
            for version in ["26.5.2", "27.0.1"] {
                assert!(qualify_macos_host(version, architecture).is_err());
            }
        }
    }

    #[test]
    fn target_matrix_is_exact_and_artifact_paths_are_platform_specific() {
        let targets = supported_bundle_targets();
        assert_eq!(targets.len(), 5);
        let triples = targets
            .iter()
            .map(|target| target.target_triple.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(triples.len(), 5);
        for target in &targets {
            let launcher = launcher_path_for(target);
            let controller = runtime_controller_path_for(target);
            assert_eq!(
                target.os == "windows",
                Path::new(launcher)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
            );
            assert_eq!(
                target.os == "windows",
                Path::new(controller)
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
            );
            assert_eq!(target.os == "macos", target.minimum_os_version.is_some());
            if target.os == "windows" {
                assert!(target.target_triple.ends_with("-pc-windows-msvc"));
            }
        }
        assert_eq!(
            target_for("windows", "x64")
                .expect("Windows x64")
                .target_triple,
            "x86_64-pc-windows-msvc"
        );
        assert_eq!(
            target_for("windows", "arm64")
                .expect("Windows ARM64")
                .target_triple,
            "aarch64-pc-windows-msvc"
        );
        assert!(target_for("windows", "universal").is_err());
    }

    #[test]
    fn checked_in_target_matrix_matches_the_runtime_contract() {
        let document: serde_json::Value = serde_json::from_str(include_str!(
            "../config/assisted-runtime-target-matrix.json"
        ))
        .expect("matrix JSON");
        assert_eq!(
            document
                .get("schemaVersion")
                .and_then(serde_json::Value::as_str),
            Some(BUNDLE_TARGET_MATRIX_VERSION)
        );
        let entries = document
            .get("targets")
            .and_then(serde_json::Value::as_array)
            .expect("target entries");
        assert_eq!(entries.len(), 5, "matrix must have exactly five targets");
        let observed = entries
            .iter()
            .map(|entry| {
                let object = entry.as_object().expect("matrix entry object");
                assert_eq!(object.len(), 3, "matrix entry has exact shape");
                assert!(object.contains_key("artifact"));
                assert!(object.contains_key("targetTriple"));
                assert!(object.contains_key("verificationStatus"));
                (
                    entry.get("artifact").and_then(serde_json::Value::as_str),
                    entry
                        .get("targetTriple")
                        .and_then(serde_json::Value::as_str),
                    entry
                        .get("verificationStatus")
                        .and_then(serde_json::Value::as_str),
                )
            })
            .collect::<BTreeSet<_>>();
        let expected = [
            (
                "migration-stream-local-macos-arm64",
                "aarch64-apple-darwin",
                "NATIVE_ACCEPTED",
            ),
            (
                "migration-stream-local-windows-x64",
                "x86_64-pc-windows-msvc",
                "NATIVE_LIFECYCLE_AND_RUNNER_REQUIRED",
            ),
            (
                "migration-stream-local-windows-arm64",
                "aarch64-pc-windows-msvc",
                "NATIVE_LIFECYCLE_AND_DEVICE_REQUIRED",
            ),
            (
                "migration-stream-local-linux-x64-gnu",
                "x86_64-unknown-linux-gnu",
                "NATIVE_RUNNER_REQUIRED",
            ),
            (
                "migration-stream-local-linux-arm64-gnu",
                "aarch64-unknown-linux-gnu",
                "NATIVE_DEVICE_REQUIRED",
            ),
        ]
        .into_iter()
        .map(|(artifact, triple, status)| (Some(artifact), Some(triple), Some(status)))
        .collect::<BTreeSet<_>>();
        assert_eq!(observed, expected);
    }

    #[test]
    fn windows_candidate_assembly_never_changes_target_acceptance_status() {
        let x64 = target_for("windows", "x64").expect("Windows x64 target");
        assert_eq!(
            target_bundle_assembly_enabled(&x64),
            cfg!(feature = "windows-native-acceptance-candidate")
        );
        let arm64 = target_for("windows", "arm64").expect("Windows ARM64 target");
        assert_eq!(
            target_bundle_assembly_enabled(&arm64),
            cfg!(feature = "windows-native-acceptance-candidate")
        );
        let mut malformed = arm64;
        malformed.target_triple = "x86_64-pc-windows-msvc".to_owned();
        assert!(
            !target_bundle_assembly_enabled(&malformed),
            "a mismatched Windows architecture/target tuple must fail closed"
        );
        assert!(target_bundle_assembly_enabled(
            &target_for("linux", "x64").expect("Linux target")
        ));
    }

    #[test]
    fn executable_headers_are_checked_against_the_exact_matrix_architecture() {
        let directory = tempfile::tempdir().expect("header fixture");
        let linux = directory.path().join("linux");
        let mut elf = vec![0x7f, b'E', b'L', b'F', 2, 1, 1];
        elf.resize(18, 0);
        elf.extend_from_slice(&62_u16.to_le_bytes());
        fs::write(&linux, elf).expect("ELF fixture");
        verify_platform_executable(&linux, &target_for("linux", "x64").expect("Linux x64"))
            .expect("matching ELF header");
        assert!(
            verify_platform_executable(&linux, &target_for("linux", "arm64").expect("Linux ARM64"))
                .is_err()
        );

        let windows = directory.path().join("windows");
        let mut pe = vec![0_u8; 256];
        pe[..2].copy_from_slice(b"MZ");
        pe[60..64].copy_from_slice(&64_u32.to_le_bytes());
        pe[64..68].copy_from_slice(b"PE\0\0");
        pe[68..70].copy_from_slice(&0x8664_u16.to_le_bytes());
        pe[88..90].copy_from_slice(&0x20b_u16.to_le_bytes());
        fs::write(&windows, &pe).expect("PE fixture");
        verify_platform_executable(
            &windows,
            &target_for("windows", "x64").expect("Windows x64"),
        )
        .expect("matching PE header");
        assert!(
            verify_platform_executable(
                &windows,
                &target_for("windows", "arm64").expect("Windows ARM64")
            )
            .is_err()
        );

        pe[68..70].copy_from_slice(&0xaa64_u16.to_le_bytes());
        fs::write(&windows, pe).expect("ARM64 PE fixture");
        verify_platform_executable(
            &windows,
            &target_for("windows", "arm64").expect("Windows ARM64"),
        )
        .expect("matching ARM64 PE header");
        assert!(
            verify_platform_executable(
                &windows,
                &target_for("windows", "x64").expect("Windows x64")
            )
            .is_err()
        );
    }

    #[cfg(all(windows, target_arch = "aarch64"))]
    #[test]
    fn native_windows_arm64_host_selects_only_the_exact_matrix_target() {
        let target = supported_host_target().expect("native Windows ARM64 target");
        assert_eq!(target.os, "windows");
        assert_eq!(target.architecture, "arm64");
        assert_eq!(target.target_triple, "aarch64-pc-windows-msvc");
    }

    #[cfg(all(windows, target_arch = "x86_64"))]
    #[test]
    fn native_windows_x64_host_selects_only_the_exact_matrix_target() {
        let target = supported_host_target().expect("native Windows x64 target");
        assert_eq!(target.os, "windows");
        assert_eq!(target.architecture, "x64");
        assert_eq!(target.target_triple, "x86_64-pc-windows-msvc");
    }

    #[test]
    fn minimal_manifest_satisfies_runtime_and_portable_shape_contracts() {
        let mut manifest = minimal_manifest();
        rehash(&mut manifest);
        validate_manifest(&manifest, &manifest.target).expect("valid runtime manifest");

        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../schemas/assisted-runtime-bundle.schema.json"
        ))
        .expect("bundle schema JSON");
        jsonschema::draft202012::meta::validate(&schema).expect("valid bundle meta-schema");
        let validator = jsonschema::draft202012::options()
            .build(&schema)
            .expect("bundle schema");
        let document = serde_json::to_value(&manifest).expect("manifest document");
        assert!(validator.is_valid(&document));

        let mut impossible_tuple = document;
        impossible_tuple["target"]["os"] = serde_json::Value::String("windows".to_owned());
        assert!(
            !validator.is_valid(&impossible_tuple),
            "portable schema must reject independent OS/architecture/triple mixing"
        );

        let mut macos_without_minimum = serde_json::to_value(&manifest).expect("manifest");
        macos_without_minimum["target"]
            .as_object_mut()
            .expect("target object")
            .remove("minimumOsVersion");
        assert!(
            !validator.is_valid(&macos_without_minimum),
            "portable schema must require the macOS minimum version"
        );

        let mut windows_with_posix_entrypoints = serde_json::to_value(&manifest).expect("manifest");
        windows_with_posix_entrypoints["target"]["os"] =
            serde_json::Value::String("windows".to_owned());
        windows_with_posix_entrypoints["target"]["architecture"] =
            serde_json::Value::String("x64".to_owned());
        windows_with_posix_entrypoints["target"]["targetTriple"] =
            serde_json::Value::String("x86_64-pc-windows-msvc".to_owned());
        windows_with_posix_entrypoints["target"]
            .as_object_mut()
            .expect("target object")
            .remove("minimumOsVersion");
        assert!(
            !validator.is_valid(&windows_with_posix_entrypoints),
            "portable schema must bind Windows targets to Windows entrypoints"
        );
    }

    #[test]
    fn malformed_manifest_authority_fails_closed() {
        let mut cases = Vec::new();

        let mut zero_hash = minimal_manifest();
        zero_hash.manifest_sha256 = ZERO_SHA256.to_owned();
        cases.push(zero_hash);

        let mut wrong_mode = minimal_manifest();
        wrong_mode
            .members
            .iter_mut()
            .find(|member| member.path == INTAKE_POLICY_PATH)
            .expect("intake policy")
            .mode = "0555".to_owned();
        rehash(&mut wrong_mode);
        cases.push(wrong_mode);

        let mut unsorted = minimal_manifest();
        unsorted.members.swap(0, 1);
        rehash(&mut unsorted);
        cases.push(unsorted);

        let mut case_collision = minimal_manifest();
        let mut collision = case_collision
            .members
            .iter()
            .find(|member| member.path == FRONTEND_INDEX_PATH)
            .expect("frontend index")
            .clone();
        collision.path = "web/INDEX.html".to_owned();
        case_collision.members.push(collision);
        case_collision
            .members
            .sort_by(|left, right| left.path.cmp(&right.path));
        rehash(&mut case_collision);
        cases.push(case_collision);

        let mut missing_policy = minimal_manifest();
        missing_policy
            .members
            .retain(|member| member.path != VALIDATION_POLICY_PATH);
        rehash(&mut missing_policy);
        cases.push(missing_policy);

        let mut extra_pack = minimal_manifest();
        extra_pack.members.push(BundleMember {
            path: "object-packs/unlisted/1.0.0/manifest.json".to_owned(),
            member_type: BundleMemberType::RegularFile,
            role: BundleMemberRole::ControlledPack,
            mode: "0444".to_owned(),
            size: 1,
            sha256: "c".repeat(64),
        });
        extra_pack
            .members
            .sort_by(|left, right| left.path.cmp(&right.path));
        rehash(&mut extra_pack);
        cases.push(extra_pack);

        for manifest in cases {
            assert!(
                validate_manifest(&manifest, &manifest.target).is_err(),
                "malformed authority must fail: {manifest:?}"
            );
        }
    }

    #[test]
    fn portable_schema_rejects_wrong_platform_mode_and_unsafe_path() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../schemas/assisted-runtime-bundle.schema.json"
        ))
        .expect("bundle schema JSON");
        let validator = jsonschema::draft202012::options()
            .build(&schema)
            .expect("bundle schema");
        let mut manifest = minimal_manifest();
        rehash(&mut manifest);
        let valid = serde_json::to_value(&manifest).expect("manifest document");

        let mut wrong_platform = valid.clone();
        wrong_platform["target"]["architecture"] = serde_json::json!("future-arch");
        assert!(!validator.is_valid(&wrong_platform));

        let mut wrong_mode = valid.clone();
        let member = wrong_mode["members"]
            .as_array_mut()
            .expect("members")
            .iter_mut()
            .find(|member| member["role"] == "POLICY")
            .expect("policy");
        member["mode"] = serde_json::json!("0555");
        assert!(!validator.is_valid(&wrong_mode));

        let mut unsafe_path = valid;
        unsafe_path["members"]
            .as_array_mut()
            .expect("members")
            .first_mut()
            .expect("first member")["path"] = serde_json::json!("web/../escape");
        assert!(!validator.is_valid(&unsafe_path));
    }

    #[test]
    fn materialized_bundle_replays_and_common_member_tamper_fails_closed() {
        let intact = materialize_minimal_bundle();
        let verified = verify_bundle_root_for_target(&intact.directory, &supported_test_target())
            .expect("materialized bundle");
        assert_eq!(verified.member_count, minimal_manifest().members.len());

        for path in [
            RUNTIME_CONTROLLER_PATH,
            FRONTEND_INDEX_PATH,
            INTAKE_POLICY_PATH,
            PROFILE_SCHEMA_PATH,
            CONTROLLED_PACK_PATHS[0],
        ] {
            let bundle = materialize_minimal_bundle();
            let target = bundle.directory.join(path);
            set_mode(&target, 0o644).expect("make member mutable");
            OpenOptions::new()
                .append(true)
                .open(&target)
                .expect("open member")
                .write_all(b"tamper")
                .expect("tamper member");
            let role = minimal_manifest()
                .members
                .into_iter()
                .find(|member| member.path == path)
                .expect("member role")
                .role;
            set_mode(
                &target,
                parse_mode(role.expected_mode()).expect("role mode"),
            )
            .expect("restore expected mode");
            assert!(
                verify_bundle_root_for_target(&bundle.directory, &supported_test_target()).is_err(),
                "{path} tamper must fail"
            );
        }
    }

    #[test]
    fn missing_extra_link_type_and_mode_changes_fail_closed() {
        let missing = materialize_minimal_bundle();
        let target = missing.directory.join(VALIDATION_POLICY_PATH);
        let parent = target.parent().expect("policy parent");
        set_mode(parent, 0o755).expect("open parent");
        fs::remove_file(&target).expect("remove policy");
        set_mode(parent, 0o555).expect("seal parent");
        assert!(
            verify_bundle_root_for_target(&missing.directory, &supported_test_target()).is_err()
        );

        let extra = materialize_minimal_bundle();
        set_mode(&extra.directory, 0o755).expect("open root");
        write_new_file(&extra.directory.join("unlisted.txt"), b"extra", 0o444)
            .expect("extra member");
        set_mode(&extra.directory, 0o555).expect("seal root");
        assert!(verify_bundle_root_for_target(&extra.directory, &supported_test_target()).is_err());

        #[cfg(unix)]
        {
            let linked = materialize_minimal_bundle();
            let alias = linked.parent.path().join("member-alias");
            fs::hard_link(linked.directory.join(INTAKE_POLICY_PATH), &alias)
                .expect("hard-link alias");
            assert!(
                verify_bundle_root_for_target(&linked.directory, &supported_test_target()).is_err()
            );

            let symbolic = materialize_minimal_bundle();
            set_mode(&symbolic.directory, 0o755).expect("open root");
            std::os::unix::fs::symlink(
                "OPERATOR-GUIDE.md",
                symbolic.directory.join("linked-guide"),
            )
            .expect("symbolic link");
            set_mode(&symbolic.directory, 0o555).expect("seal root");
            assert!(
                verify_bundle_root_for_target(&symbolic.directory, &supported_test_target())
                    .is_err()
            );

            let wrong_mode = materialize_minimal_bundle();
            set_mode(
                &wrong_mode.directory.join(BUNDLE_OPERATOR_GUIDE_FILENAME),
                0o644,
            )
            .expect("wrong member mode");
            assert!(
                verify_bundle_root_for_target(&wrong_mode.directory, &supported_test_target())
                    .is_err()
            );
        }
    }

    #[test]
    fn manifest_derived_graph_bound_stops_at_first_extra_directory() {
        let bundle = materialize_minimal_bundle();
        let manifest = minimal_manifest();
        let expected_directories = expected_directory_set(&manifest).expect("expected directories");
        set_mode(&bundle.directory, 0o755).expect("open root");
        let extra = bundle.directory.join("unlisted-directory");
        fs::create_dir(&extra).expect("extra directory");
        set_mode(&extra, 0o555).expect("seal extra");
        set_mode(&bundle.directory, 0o555).expect("seal root");

        let error = collect_actual_bundle_graph(
            &bundle.directory,
            manifest.members.len() + 1,
            expected_directories.len(),
        )
        .expect_err("extra directory must hit the manifest-derived cap");
        assert_eq!(error.code(), "BUNDLE_INVALID");
        assert!(error.to_string().contains("directory graph exceeds"));
    }

    #[test]
    fn deterministic_archive_rebuilds_byte_identically() {
        let bundle = materialize_minimal_bundle();
        let verification =
            verify_bundle_root_for_target(&bundle.directory, &supported_test_target())
                .expect("verified test bundle");
        let first_path = bundle.parent.path().join("first.zip");
        let second_path = bundle.parent.path().join("second.zip");
        let first = write_deterministic_archive(&bundle.directory, &first_path, &verification)
            .expect("first archive");
        let second = write_deterministic_archive(&bundle.directory, &second_path, &verification)
            .expect("second archive");

        assert_eq!(first.sha256, second.sha256);
        assert_eq!(
            fs::read(&first_path).expect("first bytes"),
            fs::read(&second_path).expect("second bytes")
        );
        remove_exact_owned_file(&first_path, &first.identity).expect("remove first archive");
        remove_exact_owned_file(&second_path, &second.identity).expect("remove second archive");
    }

    #[test]
    fn archive_no_replace_collision_preserves_existing_file() {
        let bundle = materialize_minimal_bundle();
        let verification =
            verify_bundle_root_for_target(&bundle.directory, &supported_test_target())
                .expect("verified test bundle");
        let archive_path = bundle.parent.path().join("sentinel.zip");
        write_new_file(&archive_path, b"preexisting archive\n", 0o444)
            .expect("preexisting archive");
        let before = fs::read(&archive_path).expect("archive before");

        let error = write_deterministic_archive(&bundle.directory, &archive_path, &verification)
            .expect_err("no-replace archive collision");
        assert_eq!(error.code(), "IO");
        assert_eq!(fs::read(&archive_path).expect("archive after"), before);
        let partials = fs::read_dir(bundle.parent.path())
            .expect("archive directory")
            .map(|entry| entry.expect("entry").file_name())
            .filter_map(|name| name.into_string().ok())
            .filter(|name| name.contains(".part-"))
            .collect::<Vec<_>>();
        assert!(partials.is_empty(), "partial archives must roll back");
    }

    #[test]
    fn checksum_no_replace_collision_preserves_existing_file() {
        let parent = tempfile::tempdir().expect("checksum parent");
        let archive_path = parent.path().join("bundle.zip");
        let checksum_path = parent.path().join("bundle.zip.sha256");
        write_new_file(&archive_path, b"archive", 0o444).expect("archive");
        write_new_file(&checksum_path, b"preexisting\n", 0o444).expect("preexisting checksum");
        let before = fs::read(&checksum_path).expect("checksum before");

        let error = write_archive_checksum(&checksum_path, &archive_path, &"a".repeat(64))
            .expect_err("no-replace checksum collision");
        assert_eq!(error.code(), "IO");
        assert_eq!(fs::read(&checksum_path).expect("checksum after"), before);
        let partials = fs::read_dir(parent.path())
            .expect("checksum directory")
            .map(|entry| entry.expect("entry").file_name())
            .filter_map(|name| name.into_string().ok())
            .filter(|name| name.contains(".part-"))
            .collect::<Vec<_>>();
        assert!(partials.is_empty(), "partial checksum files must roll back");
    }

    #[test]
    fn source_snapshot_replay_detects_new_and_changed_git_visible_files() {
        let repository = tempfile::tempdir().expect("source repository");
        let init = Command::new(test_git_executable())
            .args([
                "-C",
                repository.path().to_str().expect("repo path"),
                "init",
                "-q",
            ])
            .output()
            .expect("git init");
        assert!(init.status.success(), "git init failed");
        let tracked = repository.path().join("tracked.txt");
        write_new_file(&tracked, b"initial\n", 0o644).expect("tracked source");
        let add = Command::new(test_git_executable())
            .args([
                "-C",
                repository.path().to_str().expect("repo path"),
                "add",
                "--",
                "tracked.txt",
            ])
            .output()
            .expect("git add");
        assert!(add.status.success(), "git add failed");
        let git = verify_git_executable(&test_git_executable()).expect("test Git executable");
        let baseline =
            repository_source_snapshot_sha256(repository.path(), &git).expect("source snapshot");
        assert_eq!(
            baseline,
            "ec411dccc6b2c89649aa17969abc9e287463473682cbdd6b60af0420046b0b73"
        );

        fs::write(&tracked, b"changed\n").expect("change tracked source");
        assert_ne!(
            repository_source_snapshot_sha256(repository.path(), &git).expect("changed snapshot"),
            baseline
        );
        fs::write(&tracked, b"initial\n").expect("restore tracked source");
        fs::write(repository.path().join("new-source.txt"), b"new\n")
            .expect("new untracked source");
        assert_ne!(
            repository_source_snapshot_sha256(repository.path(), &git).expect("added snapshot"),
            baseline
        );
        fs::remove_file(&tracked).expect("delete tracked source");
        assert_ne!(
            repository_source_snapshot_sha256(repository.path(), &git).expect("deleted snapshot"),
            baseline
        );
    }

    #[test]
    fn verified_git_keeps_the_supplied_absolute_execution_path() {
        let path = test_git_executable();
        let git = verify_git_executable(&path).expect("verify test Git executable");
        assert_eq!(git.path, path);
    }

    #[test]
    fn exact_identity_rollback_refuses_replaced_file_and_directory() {
        let parent = tempfile::tempdir().expect("rollback parent");

        let file_path = parent.path().join("owned-file");
        write_new_file(&file_path, b"owned\n", 0o444).expect("owned file");
        let file_identity = capture_test_file_identity(&file_path);
        fs::rename(&file_path, parent.path().join("moved-owned-file")).expect("move owned file");
        let replacement = b"other\n";
        assert_eq!(replacement.len(), b"owned\n".len());
        write_new_file(&file_path, replacement, 0o444).expect("same-length replacement file");
        assert!(remove_exact_owned_file(&file_path, &file_identity).is_err());
        assert_eq!(
            fs::read(&file_path).expect("replacement file remains"),
            replacement
        );

        let directory_path = parent.path().join("owned-directory");
        let directory_identity = create_owned_directory(&directory_path).expect("owned directory");
        fs::rename(&directory_path, parent.path().join("moved-owned-directory"))
            .expect("move owned directory");
        fs::create_dir(&directory_path).expect("replacement directory");
        fs::write(directory_path.join("sentinel"), b"foreign\n").expect("replacement marker");
        assert!(remove_exact_owned_bundle_tree(&directory_path, &directory_identity).is_err());
        assert_eq!(
            fs::read(directory_path.join("sentinel")).expect("replacement marker remains"),
            b"foreign\n"
        );
    }

    #[cfg(windows)]
    #[test]
    fn bundle_directory_publication_retries_transient_contention_errors() {
        let parent = tempfile::tempdir().expect("publication parent");
        let partial = parent.path().join("bundle-partial");
        let published = parent.path().join("bundle-published");
        let identity = create_owned_directory(&partial).expect("owned partial directory");
        let attempts = std::cell::Cell::new(0_usize);
        publish_bundle_directory_noreplace_windows_with(
            &partial,
            &published,
            &identity,
            WindowsBundleDirectoryPublishRetry {
                deadline: Instant::now() + Duration::from_secs(1),
                maximum_attempts: 4,
                retry_delay: Duration::ZERO,
            },
            |source, destination| {
                attempts.set(attempts.get() + 1);
                match attempts.get() {
                    1 => Err(std::io::Error::from_raw_os_error(5)),
                    2 => Err(std::io::Error::from_raw_os_error(32)),
                    _ => crate::durability::publish_new(source, destination),
                }
            },
        )
        .expect("bounded publication after transient contention");
        assert_eq!(attempts.get(), 3);
        assert!(!partial.exists());
        verify_owned_bundle_directory(
            &published,
            &identity,
            "published test directory changed identity",
        )
        .expect("published identity");
    }

    #[cfg(windows)]
    #[test]
    fn bundle_directory_publication_retries_only_known_contention_errors() {
        for code in [5, 32] {
            assert!(windows_bundle_directory_publish_error_is_retryable(
                &std::io::Error::from_raw_os_error(code)
            ));
        }
        for code in [2, 3, 17, 33, 80, 87, 183, 1224] {
            assert!(!windows_bundle_directory_publish_error_is_retryable(
                &std::io::Error::from_raw_os_error(code)
            ));
        }
    }

    #[cfg(windows)]
    #[test]
    fn bundle_directory_publication_preserves_collisions_and_replacements() {
        let parent = tempfile::tempdir().expect("publication parent");
        let partial = parent.path().join("bundle-partial");
        let published = parent.path().join("bundle-published");
        let identity = create_owned_directory(&partial).expect("owned partial directory");
        fs::write(partial.join("owned"), b"owned").expect("owned member");
        fs::create_dir(&published).expect("foreign destination");
        fs::write(published.join("sentinel"), b"foreign").expect("foreign sentinel");

        assert!(publish_bundle_directory_noreplace(&partial, &published, &identity).is_err());
        assert_eq!(
            fs::read(published.join("sentinel")).expect("foreign sentinel remains"),
            b"foreign"
        );
        assert_eq!(
            fs::read(partial.join("owned")).expect("owned source remains"),
            b"owned"
        );

        fs::remove_dir_all(&published).expect("remove test destination");
        fs::rename(&partial, parent.path().join("moved-owned")).expect("move owned source");
        fs::create_dir(&partial).expect("replacement source");
        fs::write(partial.join("sentinel"), b"replacement").expect("replacement sentinel");
        assert!(publish_bundle_directory_noreplace(&partial, &published, &identity).is_err());
        assert!(!published.exists());
        assert_eq!(
            fs::read(partial.join("sentinel")).expect("replacement remains"),
            b"replacement"
        );
    }

    #[cfg(windows)]
    #[test]
    fn bundle_directory_publication_classifies_an_exact_moved_output() {
        let parent = tempfile::tempdir().expect("publication parent");
        let partial = parent.path().join("bundle-partial");
        let published = parent.path().join("bundle-published");
        let identity = create_owned_directory(&partial).expect("owned partial directory");
        fs::rename(&partial, &published).expect("external exact move");

        let failure = publish_bundle_directory_noreplace(&partial, &published, &identity)
            .expect_err("already moved authority must not be republished");
        assert!(failure.published);
        assert!(!partial.exists());
        verify_owned_bundle_directory(&published, &identity, "moved output changed identity")
            .expect("moved output identity");
    }

    #[cfg(windows)]
    #[test]
    fn bundle_directory_publication_classifies_an_error_after_exact_move() {
        let parent = tempfile::tempdir().expect("publication parent");
        let partial = parent.path().join("bundle-partial");
        let published = parent.path().join("bundle-published");
        let identity = create_owned_directory(&partial).expect("owned partial directory");
        let failure = publish_bundle_directory_noreplace_windows_with(
            &partial,
            &published,
            &identity,
            WindowsBundleDirectoryPublishRetry {
                deadline: Instant::now() + Duration::from_secs(1),
                maximum_attempts: 4,
                retry_delay: Duration::ZERO,
            },
            |source, destination| {
                crate::durability::publish_new(source, destination)?;
                Err(std::io::Error::from_raw_os_error(5))
            },
        )
        .expect_err("an error after the exact move must remain a failure");
        assert!(failure.published);
        assert!(!partial.exists());
        verify_owned_bundle_directory(&published, &identity, "error-path output changed identity")
            .expect("error-path output identity");
    }

    #[cfg(windows)]
    #[test]
    fn bundle_directory_publication_retry_is_strictly_bounded() {
        let parent = tempfile::tempdir().expect("publication parent");
        let partial = parent.path().join("bundle-partial");
        let published = parent.path().join("bundle-published");
        let identity = create_owned_directory(&partial).expect("owned partial directory");
        let attempts = std::cell::Cell::new(0_usize);
        let result = publish_bundle_directory_noreplace_windows_with(
            &partial,
            &published,
            &identity,
            WindowsBundleDirectoryPublishRetry {
                deadline: Instant::now() + Duration::from_secs(1),
                maximum_attempts: 4,
                retry_delay: Duration::ZERO,
            },
            |_, _| {
                attempts.set(attempts.get() + 1);
                Err(std::io::Error::from_raw_os_error(5))
            },
        );
        assert!(result.is_err());
        assert_eq!(attempts.get(), 4);
        assert!(partial.exists());
        assert!(!published.exists());
    }

    #[cfg(windows)]
    #[test]
    fn bundle_directory_publication_stops_when_destination_appears() {
        let parent = tempfile::tempdir().expect("publication parent");
        let partial = parent.path().join("bundle-partial");
        let published = parent.path().join("bundle-published");
        let identity = create_owned_directory(&partial).expect("owned partial directory");
        let attempts = std::cell::Cell::new(0_usize);
        let result = publish_bundle_directory_noreplace_windows_with(
            &partial,
            &published,
            &identity,
            WindowsBundleDirectoryPublishRetry {
                deadline: Instant::now() + Duration::from_secs(1),
                maximum_attempts: 4,
                retry_delay: Duration::ZERO,
            },
            |_, destination| {
                attempts.set(attempts.get() + 1);
                fs::create_dir(destination).expect("foreign destination");
                fs::write(destination.join("sentinel"), b"foreign").expect("foreign sentinel");
                Err(std::io::Error::from_raw_os_error(5))
            },
        );
        assert!(result.is_err());
        assert_eq!(attempts.get(), 1);
        assert_eq!(
            fs::read(published.join("sentinel")).expect("foreign destination remains"),
            b"foreign"
        );
        assert!(partial.exists());
    }

    #[test]
    fn frontend_snapshot_matches_manifest_and_rejects_hidden_or_changed_assets() {
        let parent = tempfile::tempdir().expect("frontend parent");
        let root = parent.path().join("frontend-assets");
        fs::create_dir(&root).expect("frontend root");
        let mut manifest = minimal_manifest();
        let frontend = manifest
            .members
            .iter_mut()
            .find(|member| member.path == FRONTEND_INDEX_PATH)
            .expect("frontend member");
        let bytes = b"<main>bounded frontend</main>\n";
        frontend.size = u64::try_from(bytes.len()).expect("frontend size");
        frontend.sha256 = sha256_hex(bytes);
        write_new_file(&root.join("index.html"), bytes, 0o644).expect("frontend index");

        let staged = frontend_asset_snapshot_sha256(&root).expect("staged frontend snapshot");
        assert_eq!(
            staged,
            "c6e5c80f528c564243bcf0cc4649b8f01716c49fffe527783c14db179212c8fb"
        );
        assert_eq!(
            staged,
            frontend_asset_manifest_sha256(&manifest).expect("manifest frontend snapshot")
        );
        fs::write(root.join("index.html"), b"changed\n").expect("change frontend");
        assert_ne!(
            frontend_asset_snapshot_sha256(&root).expect("changed frontend snapshot"),
            staged
        );
        fs::write(root.join(".DS_Store"), b"hidden\n").expect("hidden frontend member");
        assert!(frontend_asset_snapshot_sha256(&root).is_err());
    }

    #[test]
    fn repository_authority_replays_head_state_and_snapshot_together() {
        let repository = tempfile::tempdir().expect("authority repository");
        let init = Command::new(test_git_executable())
            .args([
                "-C",
                repository.path().to_str().expect("repo path"),
                "init",
                "-q",
            ])
            .output()
            .expect("git init");
        assert!(init.status.success(), "git init failed");
        let tracked = repository.path().join("tracked.txt");
        write_new_file(&tracked, b"initial\n", 0o644).expect("tracked source");
        run_test_git(repository.path(), &["add", "--", "tracked.txt"]);
        run_test_git(
            repository.path(),
            &[
                "-c",
                "user.name=Bundle Test",
                "-c",
                "user.email=bundle@example.invalid",
                "commit",
                "-q",
                "-m",
                "initial",
            ],
        );
        let git = verify_git_executable(&test_git_executable()).expect("test Git executable");
        let head = repository_head(repository.path(), &git).expect("initial head");
        let snapshot =
            repository_source_snapshot_sha256(repository.path(), &git).expect("initial snapshot");
        let mut request = test_assembly_request(repository.path(), &head, &snapshot);
        verify_repository_source_authority(repository.path(), &request)
            .expect("clean authority replay");

        fs::write(&tracked, b"changed\n").expect("change source");
        assert!(verify_repository_source_authority(repository.path(), &request).is_err());
        request.source_state = BundleSourceState::ExplicitUncommittedSnapshot;
        request.source_snapshot_sha256 =
            repository_source_snapshot_sha256(repository.path(), &git).expect("changed snapshot");
        verify_repository_source_authority(repository.path(), &request)
            .expect("explicit dirty replay");

        run_test_git(repository.path(), &["add", "--", "tracked.txt"]);
        assert!(verify_repository_source_authority(repository.path(), &request).is_err());
        run_test_git(
            repository.path(),
            &[
                "-c",
                "user.name=Bundle Test",
                "-c",
                "user.email=bundle@example.invalid",
                "commit",
                "-q",
                "-m",
                "changed",
            ],
        );
        assert!(verify_repository_source_authority(repository.path(), &request).is_err());
    }

    #[test]
    fn reopened_archive_replay_requires_original_published_identity() {
        let bundle = materialize_minimal_bundle();
        let verification =
            verify_bundle_root_for_target(&bundle.directory, &supported_test_target())
                .expect("verified test bundle");
        let archive_path = bundle.parent.path().join("owned.zip");
        let archive = write_deterministic_archive(&bundle.directory, &archive_path, &verification)
            .expect("owned archive");
        let archive_bytes = fs::read(&archive_path).expect("archive bytes");
        let moved_path = bundle.parent.path().join("moved-owned.zip");
        fs::rename(&archive_path, &moved_path).expect("move original archive");
        write_new_file(&archive_path, &archive_bytes, 0o644).expect("replacement archive");

        assert!(
            verify_and_hash_deterministic_archive(
                &archive_path,
                &bundle.directory,
                &verification,
                &archive.identity,
            )
            .is_err()
        );
        remove_exact_owned_file(&moved_path, &archive.identity).expect("remove moved archive");
    }

    fn test_git_executable() -> PathBuf {
        #[cfg(unix)]
        {
            PathBuf::from("/usr/bin/git")
        }
        #[cfg(windows)]
        {
            let output = Command::new("where.exe")
                .arg("git")
                .output()
                .expect("locate test Git executable");
            assert!(
                output.status.success(),
                "Git must be installed for bundle tests"
            );
            let first = String::from_utf8(output.stdout)
                .expect("Git path UTF-8")
                .lines()
                .next()
                .expect("one Git executable path")
                .trim()
                .to_owned();
            let path = PathBuf::from(first);
            assert!(path.is_absolute(), "Git path must be absolute");
            let resolved_is_hard_link = fs::File::open(&path)
                .and_then(|file| crate::durability::file_link_count(&file))
                .expect("test Git link count")
                > 1;
            if resolved_is_hard_link
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|name| name.eq_ignore_ascii_case("cmd"))
                && let Some(root) = path.parent().and_then(Path::parent)
            {
                let process_git = root.join("bin/git.exe");
                if process_git.is_file() {
                    return process_git;
                }
            }
            path
        }
    }

    fn run_test_git(repository: &Path, arguments: &[&str]) {
        let output = Command::new(test_git_executable())
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()
            .expect("run test Git command");
        assert!(
            output.status.success(),
            "Git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn test_assembly_request(
        repository: &Path,
        product_commit: &str,
        source_snapshot_sha256: &str,
    ) -> BundleAssemblyRequest {
        BundleAssemblyRequest {
            repository_root: repository.to_path_buf(),
            git_executable: test_git_executable(),
            frontend_assets_root: repository.join("unused-frontend"),
            release_binaries_root: repository.join("unused-release-binaries"),
            output_directory: repository.join("unused-output"),
            product_commit: product_commit.to_owned(),
            source_state: BundleSourceState::Clean,
            source_snapshot_sha256: source_snapshot_sha256.to_owned(),
            frontend_assets_sha256: "f".repeat(64),
            create_zip_archive: false,
        }
    }

    fn capture_test_file_identity(path: &Path) -> OwnedPathIdentity {
        let file = File::open(path).expect("open test file identity");
        capture_open_file_identity(path, &file).expect("capture test file identity")
    }

    fn engineering_pe(machine: u16) -> Vec<u8> {
        let mut pe = vec![0_u8; 256];
        pe[..2].copy_from_slice(b"MZ");
        pe[60..64].copy_from_slice(&64_u32.to_le_bytes());
        pe[64..68].copy_from_slice(b"PE\0\0");
        pe[68..70].copy_from_slice(&machine.to_le_bytes());
        pe[88..90].copy_from_slice(&0x20b_u16.to_le_bytes());
        pe
    }

    fn engineering_fixture() -> (TestBundle, WindowsEngineeringLayoutRequest) {
        let parent = tempfile::tempdir().expect("synthetic parent");
        let directory = parent.path().join("runtime");
        create_owned_directory(&directory).expect("runtime directory");
        let mut manifest = minimal_manifest();
        manifest.target = target_for("windows", "x64").expect("Windows target");
        manifest.entrypoints = BundleEntrypoints {
            launcher: WINDOWS_LAUNCHER_PATH.into(),
            runtime_controller: WINDOWS_RUNTIME_CONTROLLER_PATH.into(),
            integrity_cli: WINDOWS_CLI_PATH.into(),
            workbench: WINDOWS_WORKBENCH_PATH.into(),
        };
        for member in &mut manifest.members {
            member.path = match member.role {
                BundleMemberRole::PlatformLauncher => WINDOWS_LAUNCHER_PATH.into(),
                BundleMemberRole::RuntimeController => WINDOWS_RUNTIME_CONTROLLER_PATH.into(),
                BundleMemberRole::IntegrityCli => WINDOWS_CLI_PATH.into(),
                BundleMemberRole::Workbench => WINDOWS_WORKBENCH_PATH.into(),
                _ => member.path.clone(),
            };
            let bytes = if matches!(
                member.role,
                BundleMemberRole::PlatformLauncher
                    | BundleMemberRole::RuntimeController
                    | BundleMemberRole::IntegrityCli
                    | BundleMemberRole::Workbench
            ) {
                engineering_pe(0x8664)
            } else {
                b"synthetic member\n".to_vec()
            };
            member.size = u64::try_from(bytes.len()).expect("small fixture");
            member.sha256 = sha256_hex(&bytes);
            let path = directory.join(&member.path);
            create_directory_tree(&directory, path.parent().expect("parent"))
                .expect("member parents");
            write_new_file(&path, &bytes, parse_mode(&member.mode).expect("mode")).expect("member");
        }
        manifest.members.sort_by(|a, b| a.path.cmp(&b.path));
        rehash(&mut manifest);
        write_new_file(
            &directory.join(BUNDLE_MANIFEST_FILENAME),
            &canonical_json_bytes(&serde_json::to_value(manifest).expect("value"))
                .expect("canonical"),
            0o444,
        )
        .expect("manifest");
        seal_bundle_directories(&directory).expect("seal");
        let launcher = parent.path().join("trusted.exe");
        let pe = engineering_pe(0x8664);
        write_new_file(&launcher, &pe, 0o555).expect("launcher");
        let request = WindowsEngineeringLayoutRequest {
            launcher,
            expected_launcher_sha256: sha256_hex(&pe),
            runtime: directory.clone(),
            output_directory: parent.path().join("engineering"),
            engineering_policy: "local-unsigned-engineering".into(),
        };
        (TestBundle { directory, parent }, request)
    }

    fn engineering_verify(path: &Path) -> Result<BundleVerification> {
        verify_bundle_root_for_target(path, &target_for("windows", "x64")?)
    }

    #[test]
    fn windows_engineering_layout_preserves_real_synthetic_manifest_and_graph() {
        let (_fixture, request) = engineering_fixture();
        let original = engineering_snapshot(&request.runtime).expect("snapshot");
        let mut calls = Vec::new();
        let result = assemble_windows_engineering_layout_with(&request, |path| {
            calls.push(path.to_path_buf());
            engineering_verify(path)
        })
        .expect("synthetic assembly");
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0], request.runtime.canonicalize().expect("canonical"));
        assert_eq!(calls[2], calls[0]);
        assert_eq!(
            calls[3],
            request
                .output_directory
                .canonicalize()
                .expect("output")
                .join("Runtime")
        );
        let copied = engineering_snapshot(&result.runtime.bundle_root).expect("copied snapshot");
        assert!(engineering_same_content(&original, &copied));
        assert_eq!(
            result.document_launcher_sha256,
            request.expected_launcher_sha256
        );
        assert_eq!(
            fs::read_dir(&result.output_directory)
                .expect("outer graph")
                .count(),
            2
        );
        assert!(!request.runtime.join(ENGINEERING_LAUNCHER).exists());
    }

    #[test]
    fn windows_engineering_layout_rejects_policy_paths_overlap_and_existing_output() {
        let (_fixture, mut request) = engineering_fixture();
        request.engineering_policy = "release".into();
        assert!(
            assemble_windows_engineering_layout_with(&request, |_| panic!("no verifier")).is_err()
        );
        request.engineering_policy = "local-unsigned-engineering".into();
        let output = request.output_directory.clone();
        request.output_directory = PathBuf::from("relative");
        assert!(
            assemble_windows_engineering_layout_with(&request, |_| panic!("no verifier")).is_err()
        );
        request.output_directory = request.runtime.join("child");
        assert!(
            assemble_windows_engineering_layout_with(&request, |_| panic!("no verifier")).is_err()
        );
        request.output_directory = output;
        fs::create_dir(&request.output_directory).expect("foreign output");
        fs::write(request.output_directory.join("sentinel"), b"keep").expect("sentinel");
        assert!(
            assemble_windows_engineering_layout_with(&request, |_| panic!("no verifier")).is_err()
        );
        assert_eq!(
            fs::read(request.output_directory.join("sentinel")).expect("untouched"),
            b"keep"
        );
    }

    #[test]
    fn windows_engineering_layout_captured_bytes_bind_exact_architecture() {
        let x64 = target_for("windows", "x64").expect("x64 target");
        let arm64 = target_for("windows", "arm64").expect("ARM64 target");
        let earlier_named_bytes = engineering_pe(0x8664);
        verify_engineering_launcher_bytes(&earlier_named_bytes, &x64).expect("earlier x64 version");
        // A valid earlier path proof cannot authorize a different captured buffer.
        let captured = engineering_pe(0xaa64);
        assert_ne!(sha256_hex(&earlier_named_bytes), sha256_hex(&captured));
        assert!(verify_engineering_launcher_bytes(&captured, &x64).is_err());
        verify_engineering_launcher_bytes(&captured, &arm64).expect("same capture is ARM64");
        assert!(verify_engineering_launcher_bytes(&earlier_named_bytes, &arm64).is_err());
    }

    #[test]
    fn windows_engineering_layout_captured_bytes_refuse_malformed_pe_contract() {
        let target = target_for("windows", "x64").expect("target");
        let valid = engineering_pe(0x8664);
        let mut invalid = vec![valid[..255].to_vec()];
        for (offset, value) in [(0, b'X'), (64, b'X'), (88, 0)] {
            let mut bytes = valid.clone();
            bytes[offset] = value;
            invalid.push(bytes);
        }
        for offset in [250_u32, 1_048_577, u32::MAX] {
            let mut bytes = valid.clone();
            bytes[60..64].copy_from_slice(&offset.to_le_bytes());
            invalid.push(bytes);
        }
        for captured in invalid {
            assert!(verify_engineering_launcher_bytes(&captured, &target).is_err());
        }
    }

    #[test]
    fn windows_engineering_layout_refuses_digest_architecture_and_truncated_pe() {
        let mut bad_offset = engineering_pe(0x8664);
        bad_offset[60..64].copy_from_slice(&1_048_577_u32.to_le_bytes());
        for bytes in [engineering_pe(0xaa64), vec![0_u8; 10], bad_offset] {
            let (_fixture, mut request) = engineering_fixture();
            set_mode(&request.launcher, 0o644).expect("writable fixture");
            fs::write(&request.launcher, &bytes).expect("invalid fixture");
            request.expected_launcher_sha256 = sha256_hex(&bytes);
            assert!(
                assemble_windows_engineering_layout_with(&request, engineering_verify).is_err()
            );
            assert!(!request.output_directory.exists());
        }
        let (_fixture, mut request) = engineering_fixture();
        request.expected_launcher_sha256 = "0".repeat(64);
        assert!(assemble_windows_engineering_layout_with(&request, engineering_verify).is_err());
        assert!(!request.output_directory.exists());
    }

    #[test]
    fn windows_engineering_layout_refuses_verifier_failure_before_publication() {
        for fail_at in [0, 1, 2, 3] {
            let (_fixture, request) = engineering_fixture();
            let mut call = 0;
            let result = assemble_windows_engineering_layout_with(&request, |path| {
                let current = call;
                call += 1;
                if current == fail_at {
                    return Err(bundle_invalid("injected verifier refusal"));
                }
                engineering_verify(path)
            });
            assert!(result.is_err());
            assert_eq!(
                request.output_directory.exists(),
                fail_at == 3,
                "visible output preserved after move"
            );
            engineering_verify(&request.runtime).expect("source intact");
        }
    }

    #[test]
    fn windows_engineering_layout_compares_every_verification_field_except_root() {
        let (_fixture, request) = engineering_fixture();
        let source = engineering_verify(&request.runtime).expect("source");
        let mutations: [fn(&mut BundleVerification); 12] = [
            |v| v.manifest_sha256.push('x'),
            |v| v.product_commit.push('x'),
            |v| v.product_version.push('x'),
            |v| v.source_state = BundleSourceState::ExplicitUncommittedSnapshot,
            |v| v.source_snapshot_sha256.push('x'),
            |v| v.target.architecture.push('x'),
            |v| v.launcher_sha256.push('x'),
            |v| v.runtime_controller_sha256.push('x'),
            |v| v.integrity_cli_sha256.push('x'),
            |v| v.workbench_sha256.push('x'),
            |v| v.member_count += 1,
            |v| v.total_member_bytes += 1,
        ];
        for mutate in mutations {
            let mut changed = source.clone();
            mutate(&mut changed);
            assert!(engineering_same_verification(&source, &changed).is_err());
        }
        let mut relocated = source.clone();
        relocated.bundle_root = PathBuf::from("relocated");
        engineering_same_verification(&source, &relocated).expect("root only may differ");
    }

    #[test]
    fn windows_engineering_layout_refuses_source_swap_even_with_identical_bytes() {
        let (_fixture, request) = engineering_fixture();
        let source = engineering_verify(&request.runtime).expect("source");
        let mut calls = 0;
        assert!(
            assemble_windows_engineering_layout_with(&request, |_| {
                calls += 1;
                if calls == 2 {
                    set_mode(&request.launcher, 0o755)?;
                    let moved = request.launcher.with_extension("old");
                    fs::rename(&request.launcher, &moved)
                        .map_err(|e| KernelError::io(&moved, e))?;
                    write_new_file(&request.launcher, &engineering_pe(0x8664), 0o555)?;
                }
                Ok(source.clone())
            })
            .is_err()
        );
        assert!(!request.output_directory.exists());
    }

    #[test]
    fn windows_engineering_layout_refuses_staged_mutation_even_if_injected_verifier_accepts() {
        let (_fixture, request) = engineering_fixture();
        let source = engineering_verify(&request.runtime).expect("source");
        let mut calls = 0;
        assert!(
            assemble_windows_engineering_layout_with(&request, |path| {
                calls += 1;
                if calls == 2 {
                    let member = path.join(WINDOWS_RUNTIME_CONTROLLER_PATH);
                    set_mode(&member, 0o644)?;
                    fs::write(&member, b"changed").map_err(|e| KernelError::io(&member, e))?;
                }
                Ok(source.clone())
            })
            .is_err()
        );
        assert!(!request.output_directory.exists());
    }

    #[test]
    fn windows_engineering_layout_collision_after_staging_preserves_foreign_output() {
        let (_fixture, request) = engineering_fixture();
        let mut calls = 0;
        assert!(
            assemble_windows_engineering_layout_with(&request, |path| {
                calls += 1;
                if calls == 2 {
                    fs::create_dir(&request.output_directory).expect("concurrent output");
                    fs::write(request.output_directory.join("sentinel"), b"foreign")
                        .expect("foreign sentinel");
                }
                engineering_verify(path)
            })
            .is_err()
        );
        assert_eq!(
            fs::read(request.output_directory.join("sentinel")).expect("preserved"),
            b"foreign"
        );
    }

    #[test]
    fn windows_engineering_layout_extra_outer_material_is_refused() {
        let (_fixture, request) = engineering_fixture();
        let mut calls = 0;
        assert!(
            assemble_windows_engineering_layout_with(&request, |path| {
                calls += 1;
                if calls == 2 {
                    fs::write(
                        path.parent().expect("stage").join("unexpected"),
                        b"synthetic",
                    )
                    .expect("unexpected");
                }
                engineering_verify(path)
            })
            .is_err()
        );
        assert!(!request.output_directory.exists());
    }

    #[test]
    fn windows_engineering_layout_depth_count_and_file_size_are_bounded() {
        let temp = tempfile::tempdir().expect("bounded graph");
        let mut path = temp.path().to_path_buf();
        for _ in 0..=ENGINEERING_GRAPH_DEPTH {
            path = path.join("d");
            fs::create_dir(&path).expect("depth");
        }
        assert!(engineering_snapshot(temp.path()).is_err());
        let temp = tempfile::tempdir().expect("count graph");
        for n in 0..ENGINEERING_GRAPH_ENTRIES {
            fs::write(temp.path().join(format!("f{n}")), []).expect("entry");
        }
        assert!(engineering_snapshot(temp.path()).is_err());
        let temp = tempfile::tempdir().expect("oversize graph");
        let file = File::create(temp.path().join("large")).expect("sparse fixture");
        file.set_len(MAXIMUM_MEMBER_BYTES + 1)
            .expect("sparse bound");
        assert!(engineering_snapshot(temp.path()).is_err());
        let temp = tempfile::tempdir().expect("cumulative byte bound");
        fs::write(temp.path().join("one"), b"x").expect("one remaining entry");
        let mut total = MAXIMUM_BUNDLE_BYTES + MAXIMUM_MANIFEST_BYTES;
        assert!(
            engineering_snapshot_directory(
                temp.path(),
                temp.path(),
                &mut Vec::new(),
                &mut total,
                0
            )
            .is_err()
        );
    }

    #[test]
    fn windows_engineering_layout_hardlinks_are_refused_before_verifier() {
        let (_fixture, request) = engineering_fixture();
        fs::hard_link(&request.launcher, request.launcher.with_extension("alias"))
            .expect("hard link");
        assert!(
            assemble_windows_engineering_layout_with(&request, |_| panic!("no verifier")).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn windows_engineering_layout_symlink_and_dangling_output_are_refused() {
        use std::os::unix::fs::symlink;
        let (_fixture, mut request) = engineering_fixture();
        let original = request.launcher.clone();
        request.launcher = original.with_extension("link");
        symlink(&original, &request.launcher).expect("link");
        assert!(
            assemble_windows_engineering_layout_with(&request, |_| panic!("no verifier")).is_err()
        );
        request.launcher = original;
        symlink("absent", &request.output_directory).expect("dangling output");
        assert!(
            assemble_windows_engineering_layout_with(&request, |_| panic!("no verifier")).is_err()
        );
    }

    fn materialize_minimal_bundle() -> TestBundle {
        let parent = tempfile::tempdir().expect("bundle parent");
        let directory = parent.path().join("bundle");
        fs::create_dir(&directory).expect("bundle root");
        set_mode(&directory, 0o755).expect("root mode");

        let mut manifest = minimal_manifest();
        for member in &mut manifest.members {
            let bytes = member_test_bytes(member);
            member.size = u64::try_from(bytes.len()).expect("member size");
            member.sha256 = sha256_hex(&bytes);
            let path = directory.join(&member.path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("member directories");
            }
            write_new_file(
                &path,
                &bytes,
                parse_mode(member.role.expected_mode()).expect("member mode"),
            )
            .expect("member");
        }
        rehash(&mut manifest);
        let manifest_bytes =
            canonical_json_bytes(&serde_json::to_value(&manifest).expect("manifest value"))
                .expect("manifest bytes");
        write_new_file(
            &directory.join(BUNDLE_MANIFEST_FILENAME),
            &manifest_bytes,
            0o444,
        )
        .expect("manifest");
        seal_bundle_directories(&directory).expect("sealed bundle");
        TestBundle { directory, parent }
    }

    fn member_test_bytes(member: &BundleMember) -> Vec<u8> {
        if member.path == LAUNCHER_PATH {
            return PLATFORM_LAUNCHER.as_bytes().to_vec();
        }
        if matches!(
            member.role,
            BundleMemberRole::RuntimeController
                | BundleMemberRole::IntegrityCli
                | BundleMemberRole::Workbench
        ) {
            let mut bytes = vec![0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];
            bytes.extend_from_slice(&[0, 0, 0, 0, 0x02, 0, 0, 0]);
            bytes.extend_from_slice(member.path.as_bytes());
            return bytes;
        }
        format!("bounded test member: {}\n", member.path).into_bytes()
    }

    fn supported_test_target() -> BundleTarget {
        target_for(SUPPORTED_RUNTIME_OS, SUPPORTED_RUNTIME_ARCHITECTURE).expect("macOS target")
    }

    fn rehash(manifest: &mut AssistedRuntimeBundleManifest) {
        manifest.manifest_sha256 =
            calculate_manifest_self_hash(manifest).expect("manifest self hash");
    }

    fn minimal_manifest() -> AssistedRuntimeBundleManifest {
        let mut required = vec![
            (LAUNCHER_PATH, BundleMemberRole::PlatformLauncher),
            (RUNTIME_CONTROLLER_PATH, BundleMemberRole::RuntimeController),
            (CLI_PATH, BundleMemberRole::IntegrityCli),
            (WORKBENCH_PATH, BundleMemberRole::Workbench),
            (INTAKE_POLICY_PATH, BundleMemberRole::Policy),
            (PROFILING_POLICY_PATH, BundleMemberRole::Policy),
            (VALIDATION_POLICY_PATH, BundleMemberRole::Policy),
            (PROFILE_SCHEMA_PATH, BundleMemberRole::ProfileSchema),
            (MAPPING_SCHEMA_PATH, BundleMemberRole::MappingSchema),
            (BUNDLE_SCHEMA_PATH, BundleMemberRole::BundleSchema),
            (FRONTEND_INDEX_PATH, BundleMemberRole::FrontendAsset),
            (
                BUNDLE_OPERATOR_GUIDE_FILENAME,
                BundleMemberRole::OperatorGuide,
            ),
        ];
        required.extend(
            CONTROLLED_PACK_PATHS
                .into_iter()
                .map(|path| (path, BundleMemberRole::ControlledPack)),
        );
        let mut members = required
            .into_iter()
            .map(|(path, role)| BundleMember {
                path: path.to_owned(),
                member_type: BundleMemberType::RegularFile,
                role,
                mode: role.expected_mode().to_owned(),
                size: 1,
                sha256: if path == LAUNCHER_PATH {
                    sha256_hex(PLATFORM_LAUNCHER.as_bytes())
                } else {
                    "a".repeat(64)
                },
            })
            .collect::<Vec<_>>();
        members.sort_by(|left, right| left.path.cmp(&right.path));
        AssistedRuntimeBundleManifest {
            schema_version: BUNDLE_FORMAT_VERSION.to_owned(),
            canonical_json_version: CANONICAL_JSON_VERSION.to_owned(),
            manifest_sha256: ZERO_SHA256.to_owned(),
            product: BundleProduct {
                name: env!("CARGO_PKG_NAME").to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
                commit: "a".repeat(40),
                source_state: BundleSourceState::Clean,
                source_snapshot_sha256: "b".repeat(64),
            },
            target: BundleTarget {
                os: SUPPORTED_RUNTIME_OS.to_owned(),
                architecture: SUPPORTED_RUNTIME_ARCHITECTURE.to_owned(),
                target_triple: SUPPORTED_RUNTIME_TARGET_TRIPLE.to_owned(),
                minimum_os_version: Some(BUNDLED_BINARY_MINIMUM_MACOS_VERSION.to_owned()),
            },
            entrypoints: BundleEntrypoints {
                launcher: LAUNCHER_PATH.to_owned(),
                runtime_controller: RUNTIME_CONTROLLER_PATH.to_owned(),
                integrity_cli: CLI_PATH.to_owned(),
                workbench: WORKBENCH_PATH.to_owned(),
            },
            members,
        }
    }
}

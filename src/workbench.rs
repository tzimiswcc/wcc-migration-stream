//! Thin, loopback-only HTTP transport for accepted practitioner reads, the
//! explicitly enabled WP-005 decisions, and the separately enabled WP-006
//! local migration-result readiness commands.
//!
//! The adapter fixes workspace and evidence authority at process startup. Its
//! handlers perform transport validation, blocking-task orchestration, safe
//! error mapping, and response bounding only; every governed response is
//! produced by the bounded read or decision application services.

use std::{
    collections::BTreeMap,
    fs,
    io::Read as _,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Path as AxumPath, Query, State, rejection::QueryRejection},
    http::{
        HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri,
        header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, HOST, ORIGIN, REFERRER_POLICY},
    },
    middleware::{self, Next},
    response::{IntoResponse, Json},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use crate::{
    KernelError, Result,
    canonical::canonical_hash,
    decision_read_model::{DecisionAwareIssueDetail, DecisionQueueFilters, DecisionReadService},
    decision_service::DecisionService,
    decisions::{
        CorrectionOperation, CorrectionOverlayRequest, DECISION_SCHEMA_VERSION,
        DecisionConcurrency, DecisionScope, IssueDecisionRequest, IssueDisposition,
    },
    journey::{
        DownstreamJourneyAuthority, JourneyBinding, JourneyDerivation, JourneySurface, JourneyView,
        derive_journey,
    },
    object_workspace::ObjectWorkspaceService,
    publication::{PUBLICATION_COMMAND_SCHEMA, PUBLICATION_CONTRACT_VERSION},
    publication_read_model::{PUBLICATION_ELIGIBILITY_SCHEMA, PUBLICATION_LIST_SCHEMA},
    read_model::{
        CleanupFilters, IssueFilters, IssueSeverity, PageRequest, ReadService, SelectedGovernance,
    },
    reconciliation_approval::{
        RECONCILIATION_SCHEMA_VERSION, ResultActionKind, ResultActionPayload,
    },
    reconciliation_read_model::{
        ReconciliationReadService, ReconciliationRowFilters, ResultActionSummary,
    },
    reconciliation_service::{
        CalculateReconciliationRequest, CandidateCommandResult, CreateCandidateRequest,
        ReconciliationApprovalService, ReconciliationCommandResult, ResultActionCommandResult,
        ResultActionReplayProbe,
    },
};

/// Default local address for the single-workspace process.
pub const DEFAULT_LISTEN_ADDRESS: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4173);

const CONTEXT_SCHEMA: &str = "migration-stream/workbench/context/v1";
const GOVERNED_CONTEXT_SCHEMA: &str = "migration-stream/workbench/context/v2";
const RESULT_APPROVAL_CONTEXT_SCHEMA: &str = "migration-stream/workbench/context/v3";
const PUBLICATION_CONTEXT_SCHEMA: &str = "migration-stream/workbench/context/v4";
const ERROR_SCHEMA: &str = "migration-stream/http-error/v1";
const QUERY_TIMEOUT: Duration = Duration::from_secs(4);
const TASK_TIMEOUT: Duration = Duration::from_secs(5);
const MAXIMUM_QUERY_BYTES: usize = 8 * 1024;
const MAXIMUM_URI_BYTES: usize = 12 * 1024;
const MAXIMUM_HEADER_COUNT: usize = 64;
const MAXIMUM_HEADER_BYTES: usize = 32 * 1024;
const MAXIMUM_BROAD_RESPONSE_BYTES: usize = 24 * 1024 * 1024;
const MAXIMUM_ROW_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAXIMUM_FILTER_BYTES: usize = 4_096;
const MAXIMUM_ASSET_COUNT: usize = 256;
const MAXIMUM_ASSET_BYTES: usize = 32 * 1024 * 1024;
const MAXIMUM_SINGLE_ASSET_BYTES: usize = 8 * 1024 * 1024;
const MAXIMUM_COMMAND_BODY_BYTES: usize = 512 * 1024;
const MINIMUM_IDEMPOTENCY_KEY_BYTES: usize = 16;
const MAXIMUM_IDEMPOTENCY_KEY_BYTES: usize = 128;
const GOVERNED_INTENT: &str = "governed-decision-v1";
const RESULT_APPROVAL_INTENT: &str = "migration-result-approval-v1";
const READINESS_PUBLICATION_INTENT: &str = "readiness-publication-v1";
const RESULT_BOUNDARY_ACKNOWLEDGEMENT_VERSION: &str = "migration-stream-result-boundary/v1";
const CALCULATE_RECONCILIATION_COMMAND_SCHEMA: &str =
    "migration-stream/write/calculate-reconciliation/v1";
const CREATE_RESULT_CANDIDATE_COMMAND_SCHEMA: &str =
    "migration-stream/write/create-result-candidate/v1";
const APPROVE_RESULT_CANDIDATE_COMMAND_SCHEMA: &str =
    "migration-stream/write/approve-result-candidate/v1";
const REJECT_RESULT_CANDIDATE_COMMAND_SCHEMA: &str =
    "migration-stream/write/reject-result-candidate/v1";
const REVOKE_RESULT_APPROVAL_COMMAND_SCHEMA: &str =
    "migration-stream/write/revoke-result-approval/v1";
const SUPERSEDE_RESULT_APPROVAL_COMMAND_SCHEMA: &str =
    "migration-stream/write/supersede-result-approval/v1";
const RECONCILIATION_COMMAND_RESULT_SCHEMA: &str =
    "migration-stream/write/reconciliation-calculation-result/v1";
const CANDIDATE_COMMAND_RESULT_SCHEMA: &str =
    "migration-stream/write/result-candidate-command-result/v1";
const ACTION_COMMAND_RESULT_SCHEMA: &str = "migration-stream/write/result-action-command-result/v1";

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Startup-fixed authority for one local workbench process.
#[derive(Clone, Debug)]
pub struct WorkbenchConfig {
    pub workspace: PathBuf,
    pub job_id: String,
    pub profiling_run_id: Option<String>,
    pub governed_decisions: bool,
    pub migration_result_approval: bool,
    pub readiness_publication: bool,
    profile_schema: PathBuf,
    governance_selection: GovernanceSelection,
}

#[derive(Clone, Debug)]
enum GovernanceSelection {
    Latest,
    Exact(Option<SelectedGovernance>),
}

impl WorkbenchConfig {
    pub fn new(
        workspace: impl Into<PathBuf>,
        job_id: impl Into<String>,
        profiling_run_id: Option<String>,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            job_id: job_id.into(),
            profiling_run_id,
            governed_decisions: false,
            migration_result_approval: false,
            readiness_publication: false,
            profile_schema: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("schemas/migration-object-profile.schema.json"),
            governance_selection: GovernanceSelection::Latest,
        }
    }

    /// Use one startup-fixed profile schema for governed decision replay.
    ///
    /// Bundled and engagement-mode callers must provide the exact verified
    /// schema path instead of relying on the build checkout.
    #[must_use]
    pub fn with_profile_schema(mut self, profile_schema: impl Into<PathBuf>) -> Self {
        self.profile_schema = profile_schema.into();
        self
    }

    /// Require the selected validation authority to match an owning engagement.
    ///
    /// `None` explicitly requires that the job has no validation authority.
    /// This prevents a downstream surface from silently switching to a newer
    /// or stale validation run selected only by timestamp.
    #[must_use]
    pub fn with_expected_governance(mut self, expected: Option<SelectedGovernance>) -> Self {
        self.governance_selection = GovernanceSelection::Exact(expected);
        self
    }

    /// Enable only the bounded WP-005 governed decision capability.
    ///
    /// Source, profile, mapping, validation-launch, result, publication, and
    /// target writes remain disabled.
    #[must_use]
    pub const fn with_governed_decisions(mut self) -> Self {
        self.governed_decisions = true;
        self
    }

    /// Enable the bounded WP-006 local migration-result readiness capability.
    ///
    /// WP-006 consumes WP-005 authority, so this also exposes the already
    /// accepted governed-decision surface. Publication, target-system, source,
    /// profile, mapping, and validation-launch writes remain disabled.
    #[must_use]
    pub const fn with_migration_result_approval(mut self) -> Self {
        self.governed_decisions = true;
        self.migration_result_approval = true;
        self
    }

    /// Enable governed local readiness-package publication.
    ///
    /// Publication consumes the bounded WP-006 result authority, so this also
    /// exposes the existing governed-decision and migration-result surfaces.
    /// External target-system writes remain disabled.
    #[must_use]
    pub const fn with_readiness_publication(mut self) -> Self {
        self.governed_decisions = true;
        self.migration_result_approval = true;
        self.readiness_publication = true;
        self
    }
}

/// Explicit negative and positive capabilities for governed local mode.
#[derive(Clone, Debug, Serialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct WorkbenchCapabilities {
    pub governed_issue_decisions: bool,
    pub correction_overlays: bool,
    pub authenticated_practitioner_identity: bool,
    pub source_mutation: bool,
    pub original_projection_mutation: bool,
    pub profile_mutation: bool,
    pub mapping_mutation: bool,
    pub migration_result_approval: bool,
    pub target_write: bool,
    pub publication: bool,
}

/// Sanitized fixed context available to the same-origin browser.
#[derive(Clone, Debug, Serialize)]
pub struct WorkbenchContext {
    pub schema_version: &'static str,
    pub read_only: bool,
    pub workspace_label: String,
    pub job_id: String,
    pub profiling_run_id: Option<String>,
    pub selected_governance: Option<SelectedGovernance>,
    pub supported_contracts: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<WorkbenchCapabilities>,
}

#[derive(Clone)]
struct WorkbenchState {
    reads: ReadService,
    decision_reads: Option<DecisionReadService>,
    decision_writes: Option<DecisionService>,
    reconciliation_reads: Option<ReconciliationReadService>,
    reconciliation_writes: Option<ReconciliationApprovalService>,
    publication_reads: Option<crate::publication_read_model::PublicationReadService>,
    publication_writes: Option<crate::publication_service::PublicationService>,
    reconciliation_summary_available: bool,
    context: WorkbenchContext,
    journey: JourneyView,
    identity: WorkspaceIdentity,
}

#[derive(Clone)]
struct WorkspaceIdentity {
    workspace_path: PathBuf,
    database_path: PathBuf,
    workspace_handle: Arc<fs::File>,
    database_handle: Arc<fs::File>,
    workspace_id: String,
    descriptor_hash: String,
}

#[derive(Clone)]
pub(crate) struct StaticAssets {
    files: BTreeMap<String, StaticAsset>,
    index: StaticAsset,
}

#[derive(Clone)]
struct StaticAsset {
    content_type: &'static str,
    bytes: Vec<u8>,
}

impl WorkspaceIdentity {
    fn capture(
        workspace: &Path,
        workspace_record: &crate::object_workspace::ObjectWorkspaceRecord,
    ) -> Result<Self> {
        let requested =
            fs::symlink_metadata(workspace).map_err(|error| KernelError::io(workspace, error))?;
        if !requested.file_type().is_dir()
            || requested.file_type().is_symlink()
            || metadata_is_reparse_point(&requested)
        {
            return Err(KernelError::IntegrityFailed(
                "workbench workspace must be one real non-link directory".to_owned(),
            ));
        }
        let workspace_path = workspace
            .canonicalize()
            .map_err(|error| KernelError::io(workspace, error))?;
        let database_path = workspace_path.join("state.sqlite3");
        let workspace_handle = retain_named_path(&workspace_path, true)
            .map_err(|error| KernelError::io(&workspace_path, error))?;
        let database_handle = retain_named_path(&database_path, false)
            .map_err(|error| KernelError::io(&database_path, error))?;
        let identity = Self {
            workspace_path,
            database_path,
            workspace_handle: Arc::new(workspace_handle),
            database_handle: Arc::new(database_handle),
            workspace_id: workspace_record.workspace_id.clone(),
            descriptor_hash: workspace_record.descriptor_hash.clone(),
        };
        identity.verify()?;
        Ok(identity)
    }

    fn verify(&self) -> Result<()> {
        self.verify_retained_paths()?;
        self.verify_workspace_authority()
    }

    fn verify_retained_paths(&self) -> Result<()> {
        let workspace_matches =
            named_path_matches(&self.workspace_path, self.workspace_handle.as_ref(), true)
                .map_err(|error| KernelError::io(&self.workspace_path, error))?;
        let database_matches =
            named_path_matches(&self.database_path, self.database_handle.as_ref(), false)
                .map_err(|error| KernelError::io(&self.database_path, error))?;
        if !workspace_matches || !database_matches {
            return Err(KernelError::IntegrityFailed(
                "startup-selected workspace identity changed".to_owned(),
            ));
        }
        Ok(())
    }

    fn verify_workspace_authority(&self) -> Result<()> {
        let active = ObjectWorkspaceService::new(&self.workspace_path)?.require_active()?;
        if active.workspace_id != self.workspace_id
            || active.descriptor_hash != self.descriptor_hash
        {
            return Err(KernelError::IntegrityFailed(
                "startup-selected object workspace authority changed".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn retain_named_path(path: &Path, directory: bool) -> std::io::Result<fs::File> {
    let before = fs::symlink_metadata(path)?;
    if before.file_type().is_symlink()
        || (directory && !before.file_type().is_dir())
        || (!directory && !before.file_type().is_file())
    {
        return Err(std::io::Error::other(
            "retained workbench path has an unsupported type",
        ));
    }
    let file = fs::File::open(path)?;
    let opened = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    if !same_unix_identity(&before, &opened)
        || !same_unix_identity(&opened, &named)
        || named.file_type().is_symlink()
    {
        return Err(std::io::Error::other(
            "retained workbench path identity changed while opening",
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn named_path_matches(path: &Path, file: &fs::File, directory: bool) -> std::io::Result<bool> {
    let named = fs::symlink_metadata(path)?;
    let opened = file.metadata()?;
    let expected_kind = if directory {
        named.file_type().is_dir()
    } else {
        named.file_type().is_file()
    };
    Ok(expected_kind && !named.file_type().is_symlink() && same_unix_identity(&named, &opened))
}

#[cfg(unix)]
fn same_unix_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(windows)]
fn retain_named_path(path: &Path, directory: bool) -> std::io::Result<fs::File> {
    let file = if directory {
        crate::durability::open_directory_nofollow(path)?
    } else {
        crate::durability::open_file_nofollow(path)?
    };
    if named_path_matches(path, &file, directory)? {
        Ok(file)
    } else {
        Err(std::io::Error::other(
            "retained workbench path has an unsupported type or identity",
        ))
    }
}

#[cfg(windows)]
fn named_path_matches(path: &Path, file: &fs::File, directory: bool) -> std::io::Result<bool> {
    if directory {
        crate::durability::path_names_open_directory(path, file)
    } else {
        crate::durability::path_names_open_file(path, file)
    }
}

#[cfg(not(any(unix, windows)))]
fn retain_named_path(_path: &Path, _directory: bool) -> std::io::Result<fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "retained workbench identity is unsupported on this platform",
    ))
}

#[cfg(not(any(unix, windows)))]
fn named_path_matches(_path: &Path, _file: &fs::File, _directory: bool) -> std::io::Result<bool> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "retained workbench identity is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &fs::Metadata) -> bool {
    crate::durability::is_reparse_point(metadata)
}

#[cfg(not(windows))]
const fn metadata_is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

/// Reject every non-loopback listener before socket binding.
///
/// # Errors
///
/// Returns a bounded read-model error when `address` is not loopback.
pub fn validate_loopback_address(address: SocketAddr) -> Result<()> {
    if address.ip().is_loopback() {
        Ok(())
    } else {
        Err(KernelError::ReadModel(
            "workbench listener must use an explicit loopback address".to_owned(),
        ))
    }
}

/// Preflight the startup-fixed workspace and build the bounded API router.
///
/// # Errors
///
/// Returns the accepted read-service error when the workspace, schema,
/// integrity, or fixed evidence identities fail closed during preflight.
pub fn build_router(config: WorkbenchConfig) -> Result<Router> {
    let state = preflight(config)?;
    Ok(Router::new()
        .nest("/api/v1", api_routes(state))
        .fallback(route_not_found)
        .layer(middleware::from_fn(enforce_transport_bounds))
        .layer(middleware::from_fn(add_security_headers)))
}

/// Build the same-origin API and bounded in-memory workbench asset surface.
///
/// Static files are loaded once after rejecting link/special-file entries. No
/// browser request is translated into a server filesystem read.
///
/// # Errors
///
/// Returns a fail-closed workspace/read error or a static asset integrity/I/O
/// error when the bounded production build cannot be loaded safely.
pub fn build_application_router(
    config: WorkbenchConfig,
    assets_root: impl AsRef<Path>,
) -> Result<Router> {
    let state = preflight(config)?;
    let assets = StaticAssets::load(assets_root.as_ref())?;
    Ok(Router::new()
        .nest("/api/v1", api_routes(state))
        .fallback(move |method: Method, uri: Uri| {
            let assets = assets.clone();
            async move { static_fallback(&method, &uri, &assets) }
        })
        .layer(middleware::from_fn(enforce_transport_bounds))
        .layer(middleware::from_fn(add_security_headers)))
}

#[allow(clippy::too_many_lines)]
fn preflight(config: WorkbenchConfig) -> Result<WorkbenchState> {
    let object_workspace_service = ObjectWorkspaceService::new(&config.workspace)?;
    let object_workspace = object_workspace_service.inspect()?;
    let workspace_record = object_workspace_service.require_active()?;
    let requested_readiness_publication = config.readiness_publication;
    let requested_migration_result_approval = config.migration_result_approval;
    let requested_governed_decisions =
        config.governed_decisions || requested_migration_result_approval;
    if requested_governed_decisions {
        // Governed mode performs the one authorized writer preflight before
        // constructing its bounded read snapshot. Opening the writer retains
        // the existing exclusive lock, WAL/FULL, foreign-key, and migration
        // guarantees; dropping it grants no decision authority.
        drop(crate::state::WorkspaceWriter::open(&config.workspace)?);
    }
    let identity = WorkspaceIdentity::capture(&config.workspace, &workspace_record)?;
    let reads = ReadService::with_query_timeout(identity.workspace_path.clone(), QUERY_TIMEOUT)?;
    let job = reads.job_summary(&config.job_id)?;

    if job.data.state == "COMPLETE" {
        reads.file_manifest(
            &config.job_id,
            &PageRequest {
                page_size: 1,
                cursor: None,
            },
        )?;
    }

    if let Some(run_id) = config.profiling_run_id.as_deref() {
        let profiling = reads.profiling_summary(
            run_id,
            false,
            &PageRequest {
                page_size: 1,
                cursor: None,
            },
        )?;
        if profiling.scope.job_id != config.job_id {
            return Err(KernelError::ReadModel(
                "profiling run does not belong to the startup-selected job".to_owned(),
            ));
        }
    }

    if let Some(governance) = job.data.selected_governance.as_ref() {
        reads.profile_view(
            &governance.profile_id,
            &governance.profile_version,
            &governance.profile_hash,
        )?;
        reads.mapping_coverage(
            &governance.mapping_id,
            &governance.mapping_version,
            &governance.mapping_hash,
            &PageRequest {
                page_size: 1,
                cursor: None,
            },
        )?;
        let validation = reads.validation_summary(&governance.validation_run_id)?;
        if validation.data.job_id != config.job_id {
            return Err(KernelError::ReadModel(
                "selected validation run does not belong to the startup-selected job".to_owned(),
            ));
        }
        reads.reconciliation_summary(
            &config.job_id,
            Some(&governance.validation_run_id),
            &PageRequest {
                page_size: 1,
                cursor: None,
            },
        )?;
    }

    reads.operational_cleanup(
        &CleanupFilters::default(),
        &PageRequest {
            page_size: 1,
            cursor: None,
        },
    )?;

    let selected_governance = job.data.selected_governance;
    if let GovernanceSelection::Exact(expected) = &config.governance_selection
        && &selected_governance != expected
    {
        return Err(KernelError::IntegrityFailed(
            "workbench validation authority differs from the active engagement binding".to_owned(),
        ));
    }
    if requested_migration_result_approval && selected_governance.is_none() {
        return Err(KernelError::ReadModel(
            "migration-result readiness requires startup-selected canonical governance".to_owned(),
        ));
    }
    let entity_count = selected_governance
        .as_ref()
        .map(|governance| {
            let profile = reads.profile_view(
                &governance.profile_id,
                &governance.profile_version,
                &governance.profile_hash,
            )?;
            let count = profile
                .data
                .document
                .get("entities")
                .and_then(serde_json::Value::as_array)
                .map(Vec::len)
                .ok_or_else(|| {
                    KernelError::ReadModel(
                        "journey profile binding has no canonical entity scope".to_owned(),
                    )
                })?;
            u64::try_from(count).map_err(|_| {
                KernelError::ReadModel(
                    "journey profile entity count exceeds its contract".to_owned(),
                )
            })
        })
        .transpose()?;
    let multi_entity_downstream_locked = selected_governance.is_some() && entity_count != Some(1);
    let governed_decisions = requested_governed_decisions && !multi_entity_downstream_locked;
    let migration_result_approval =
        requested_migration_result_approval && !multi_entity_downstream_locked;
    let readiness_publication = requested_readiness_publication && !multi_entity_downstream_locked;
    let (decision_reads, decision_writes) = if governed_decisions {
        let decision_reads = DecisionReadService::new(identity.workspace_path.clone());
        if let Some(governance) = selected_governance.as_ref() {
            decision_reads.decision_queue(
                &config.job_id,
                &governance.validation_run_id,
                &DecisionQueueFilters::default(),
                &PageRequest {
                    page_size: 1,
                    cursor: None,
                },
            )?;
        }
        (
            Some(decision_reads),
            Some(DecisionService::new(
                identity.workspace_path.clone(),
                config.profile_schema.clone(),
            )),
        )
    } else {
        (None, None)
    };

    let (reconciliation_reads, reconciliation_writes) = if migration_result_approval {
        let reconciliation_reads = ReconciliationReadService::new(identity.workspace_path.clone())
            .with_query_timeout(QUERY_TIMEOUT)?;
        reconciliation_reads.snapshot_list(
            &config.job_id,
            &PageRequest {
                page_size: 1,
                cursor: None,
            },
        )?;
        (
            Some(reconciliation_reads),
            Some(ReconciliationApprovalService::new(
                identity.workspace_path.clone(),
            )),
        )
    } else {
        (None, None)
    };

    let (publication_reads, publication_writes) = if readiness_publication {
        (
            Some(crate::publication_read_model::PublicationReadService::new(
                identity.workspace_path.clone(),
            )),
            Some(crate::publication_service::PublicationService::new(
                identity.workspace_path.clone(),
            )),
        )
    } else {
        (None, None)
    };

    let validation = selected_governance
        .as_ref()
        .map(|governance| reads.validation_summary(&governance.validation_run_id))
        .transpose()?;
    let source_hash = job
        .data
        .accepted_source
        .as_ref()
        .map(|source| source.source_hash.clone());
    let validation_complete = validation.as_ref().is_some_and(|summary| {
        summary.data.state == "COMPLETE" && summary.data.result_hash.is_some()
    });
    let issues_available = validation
        .as_ref()
        .is_some_and(|summary| summary.data.issue_count > 0);
    let journey = derive_journey(
        JourneyDerivation {
            binding: JourneyBinding {
                job_id: Some(config.job_id.clone()),
                source_hash,
                profiling_run_id: config.profiling_run_id.clone(),
                profile_id: selected_governance
                    .as_ref()
                    .map(|governance| governance.profile_id.clone()),
                profile_version: selected_governance
                    .as_ref()
                    .map(|governance| governance.profile_version.clone()),
                profile_hash: selected_governance
                    .as_ref()
                    .map(|governance| governance.profile_hash.clone()),
                mapping_id: selected_governance
                    .as_ref()
                    .map(|governance| governance.mapping_id.clone()),
                mapping_version: selected_governance
                    .as_ref()
                    .map(|governance| governance.mapping_version.clone()),
                mapping_hash: selected_governance
                    .as_ref()
                    .map(|governance| governance.mapping_hash.clone()),
                validation_run_id: selected_governance
                    .as_ref()
                    .map(|governance| governance.validation_run_id.clone()),
                validation_result_hash: validation
                    .as_ref()
                    .and_then(|summary| summary.data.result_hash.clone()),
                entity_count,
                ..JourneyBinding::default()
            },
            surface: JourneySurface::Workbench,
            engagement_exists: false,
            source_complete: job.data.state == "COMPLETE",
            destination_complete: selected_governance.is_some(),
            mapping_complete: selected_governance.is_some(),
            operator_governed_mapping: false,
            validation_complete,
            issues_available,
            comparison_available: false,
            comparison_pending: false,
            comparison_complete: false,
            downstream: if multi_entity_downstream_locked {
                DownstreamJourneyAuthority::MultiEntityLocked
            } else if governed_decisions {
                DownstreamJourneyAuthority::Enabled
            } else {
                DownstreamJourneyAuthority::Disabled
            },
            decisions_available: governed_decisions && issues_available,
            // The legacy reconciliation page is a bounded read in every
            // workbench mode. Migration-result approval remains independently
            // capability-gated by the existing context and command handlers.
            reconciliation_available: !multi_entity_downstream_locked,
            package_available: readiness_publication,
        },
        object_workspace,
    );
    let context = WorkbenchContext {
        schema_version: if readiness_publication {
            PUBLICATION_CONTEXT_SCHEMA
        } else if migration_result_approval {
            RESULT_APPROVAL_CONTEXT_SCHEMA
        } else if governed_decisions {
            GOVERNED_CONTEXT_SCHEMA
        } else {
            CONTEXT_SCHEMA
        },
        read_only: !governed_decisions,
        workspace_label: workspace_record.display_name,
        job_id: config.job_id,
        profiling_run_id: config.profiling_run_id,
        selected_governance,
        supported_contracts: supported_contracts(
            governed_decisions,
            migration_result_approval,
            readiness_publication,
        ),
        capabilities: governed_decisions.then_some(WorkbenchCapabilities {
            governed_issue_decisions: true,
            correction_overlays: true,
            authenticated_practitioner_identity: false,
            source_mutation: false,
            original_projection_mutation: false,
            profile_mutation: false,
            mapping_mutation: false,
            migration_result_approval,
            target_write: false,
            publication: readiness_publication,
        }),
    };
    identity.verify()?;
    Ok(WorkbenchState {
        reads,
        decision_reads,
        decision_writes,
        reconciliation_reads,
        reconciliation_writes,
        publication_reads,
        publication_writes,
        reconciliation_summary_available: !multi_entity_downstream_locked,
        context,
        journey,
        identity,
    })
}

fn api_routes(state: WorkbenchState) -> Router {
    Router::new()
        .route("/context", get(context))
        .route("/journey", get(journey))
        .route("/job", get(job))
        .route("/files", get(files))
        .route("/profiling", get(profiling))
        .route("/profile", get(profile))
        .route("/mapping", get(mapping))
        .route("/validation", get(validation))
        .route("/issues", get(issues))
        .route("/decision-queue", get(decision_queue))
        .route("/decision-counts", get(decision_counts))
        .route("/issues/{issue_id}/decision-detail", get(decision_issue))
        .route("/issues/{issue_id}/decisions", get(decision_history))
        .route("/issues/{issue_id}/decisions/correct", post(correct_issue))
        .route("/issues/{issue_id}/decisions/waive", post(waive_issue))
        .route(
            "/issues/{issue_id}/decisions/accept-as-is",
            post(accept_issue_as_is),
        )
        .route(
            "/issues/{issue_id}/decisions/exclude-row",
            post(exclude_issue_row),
        )
        .route("/corrections/{overlay_id}", get(correction_detail))
        .route("/decisions/{decision_id}", get(decision_result))
        .route(
            "/result-reconciliation/snapshots",
            get(result_reconciliation_snapshots),
        )
        .route(
            "/result-reconciliation/snapshots/{snapshot_id}",
            get(result_reconciliation_snapshot),
        )
        .route(
            "/result-reconciliation/snapshots/{snapshot_id}/rows",
            get(result_reconciliation_rows),
        )
        .route(
            "/result-reconciliation/snapshots/{snapshot_id}/rows/{source_ordinal}",
            get(result_reconciliation_row_detail),
        )
        .route("/result-candidates", get(result_candidates))
        .route(
            "/result-candidates/{candidate_id}/eligibility",
            get(result_candidate_eligibility),
        )
        .route("/result-actions", get(result_actions))
        .route(
            "/result-reconciliation/calculate",
            post(calculate_result_reconciliation),
        )
        .route("/result-candidates/create", post(create_result_candidate))
        .route(
            "/result-candidates/{candidate_id}/approve",
            post(approve_result_candidate),
        )
        .route(
            "/result-candidates/{candidate_id}/reject",
            post(reject_result_candidate),
        )
        .route(
            "/result-candidates/{candidate_id}/revoke",
            post(revoke_result_approval),
        )
        .route(
            "/result-candidates/{candidate_id}/supersede",
            post(supersede_result_approval),
        )
        .route("/rows/{source_ordinal}", get(row_detail))
        .route("/reconciliation", get(reconciliation))
        .route("/operations", get(operations))
        .route("/publications/eligibility", get(publication_eligibility))
        .route(
            "/publications",
            post(create_publication).get(list_publications),
        )
        .route(
            "/publications/{publication_id}",
            get(get_publication_detail),
        )
        .route(
            "/publications/{publication_id}/download",
            get(download_publication),
        )
        .method_not_allowed_fallback(method_not_allowed)
        .fallback(route_not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            enforce_workspace_identity,
        ))
        .with_state(state)
}

impl StaticAssets {
    pub(crate) fn load(root: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(root).map_err(|error| KernelError::io(root, error))?;
        if !metadata.file_type().is_dir()
            || metadata.file_type().is_symlink()
            || metadata_is_reparse_point(&metadata)
        {
            return Err(KernelError::IntegrityFailed(
                "workbench asset root must be one real non-link directory".to_owned(),
            ));
        }
        let mut files = BTreeMap::new();
        let mut total_bytes = 0_usize;
        load_asset_directory(root, root, &mut files, &mut total_bytes)?;
        let index = files.get("index.html").cloned().ok_or_else(|| {
            KernelError::IntegrityFailed(
                "workbench production assets do not contain index.html".to_owned(),
            )
        })?;
        Ok(Self { files, index })
    }
}

fn load_asset_directory(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, StaticAsset>,
    total_bytes: &mut usize,
) -> Result<()> {
    let directory_handle =
        retain_named_path(directory, true).map_err(|error| KernelError::io(directory, error))?;
    let mut entries = fs::read_dir(directory)
        .map_err(|error| KernelError::io(directory, error))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| KernelError::io(directory, error))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|error| KernelError::io(&path, error))?;
        if metadata.file_type().is_symlink() || metadata_is_reparse_point(&metadata) {
            return Err(KernelError::IntegrityFailed(
                "workbench production assets cannot contain links".to_owned(),
            ));
        }
        if metadata.file_type().is_dir() {
            load_asset_directory(root, &path, files, total_bytes)?;
            continue;
        }
        if !metadata.file_type().is_file() || files.len() >= MAXIMUM_ASSET_COUNT {
            return Err(KernelError::IntegrityFailed(
                "workbench production assets exceed their file/type bound".to_owned(),
            ));
        }
        let length = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
        if length > MAXIMUM_SINGLE_ASSET_BYTES {
            return Err(KernelError::IntegrityFailed(
                "one workbench production asset exceeds its byte bound".to_owned(),
            ));
        }
        *total_bytes = total_bytes.checked_add(length).ok_or_else(|| {
            KernelError::IntegrityFailed("workbench asset byte count overflow".to_owned())
        })?;
        if *total_bytes > MAXIMUM_ASSET_BYTES {
            return Err(KernelError::IntegrityFailed(
                "workbench production assets exceed their total byte bound".to_owned(),
            ));
        }
        let relative = path.strip_prefix(root).map_err(|_| {
            KernelError::IntegrityFailed("workbench asset escaped its root".to_owned())
        })?;
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(KernelError::IntegrityFailed(
                "workbench asset has an unsafe relative path".to_owned(),
            ));
        }
        let name = relative.to_str().ok_or_else(|| {
            KernelError::IntegrityFailed("workbench asset path is not UTF-8".to_owned())
        })?;
        let name = name.replace('\\', "/");
        let content_type = asset_content_type(relative)?;
        let file =
            retain_named_path(&path, false).map_err(|error| KernelError::io(&path, error))?;
        let mut bytes = Vec::with_capacity(length);
        file.take(u64::try_from(MAXIMUM_SINGLE_ASSET_BYTES + 1).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map_err(|error| KernelError::io(&path, error))?;
        if bytes.len() != length {
            return Err(KernelError::IntegrityFailed(
                "workbench asset identity changed while loading".to_owned(),
            ));
        }
        files.insert(
            name,
            StaticAsset {
                content_type,
                bytes,
            },
        );
    }
    if !named_path_matches(directory, &directory_handle, true)
        .map_err(|error| KernelError::io(directory, error))?
    {
        return Err(KernelError::IntegrityFailed(
            "workbench asset directory identity changed while loading".to_owned(),
        ));
    }
    Ok(())
}

fn asset_content_type(path: &Path) -> Result<&'static str> {
    match path.extension().and_then(|value| value.to_str()) {
        Some("html") => Ok("text/html; charset=utf-8"),
        Some("js" | "mjs") => Ok("text/javascript; charset=utf-8"),
        Some("css") => Ok("text/css; charset=utf-8"),
        Some("json") => Ok("application/json"),
        Some("svg") => Ok("image/svg+xml"),
        Some("png") => Ok("image/png"),
        Some("ico") => Ok("image/x-icon"),
        Some("woff") => Ok("font/woff"),
        Some("woff2") => Ok("font/woff2"),
        _ => Err(KernelError::IntegrityFailed(
            "workbench production asset has an unsupported extension".to_owned(),
        )),
    }
}

pub(crate) fn static_fallback(
    method: &Method,
    uri: &Uri,
    assets: &StaticAssets,
) -> axum::response::Response {
    if !matches!(method, &Method::GET | &Method::HEAD) {
        return ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "METHOD_NOT_ALLOWED",
            "The workbench surface exposes read-only GET operations only.",
            false,
        )
        .into_response();
    }
    let path = uri.path();
    if path == "/api" || path.starts_with("/api/") {
        return ApiError::new(
            StatusCode::NOT_FOUND,
            "API_ROUTE_NOT_FOUND",
            "The requested local API route does not exist.",
            false,
        )
        .into_response();
    }
    let requested = path.trim_start_matches('/');
    let asset = if requested.is_empty() {
        Some(&assets.index)
    } else {
        assets.files.get(requested)
    };
    if let Some(asset) = asset {
        return asset_response(method, asset);
    }
    let final_component = requested.rsplit('/').next().unwrap_or_default();
    if !final_component.contains('.') {
        return asset_response(method, &assets.index);
    }
    ApiError::new(
        StatusCode::NOT_FOUND,
        "ASSET_NOT_FOUND",
        "The requested workbench asset does not exist.",
        false,
    )
    .into_response()
}

fn asset_response(method: &Method, asset: &StaticAsset) -> axum::response::Response {
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(asset.bytes.clone())
    };
    let mut response = Response::new(body);
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(asset.content_type));
    response
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    page_size: Option<u32>,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyQuery {}

impl PageQuery {
    fn page(self) -> std::result::Result<PageRequest, ApiError> {
        let page = PageRequest {
            page_size: self.page_size.unwrap_or(50),
            cursor: self.cursor,
        };
        validate_page_transport(&page)?;
        Ok(page)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfilingQuery {
    page_size: Option<u32>,
    cursor: Option<String>,
    #[serde(default)]
    include_samples: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IssueQuery {
    page_size: Option<u32>,
    cursor: Option<String>,
    severity: Option<IssueSeverity>,
    rule_id: Option<String>,
    entity_id: Option<String>,
    field_id: Option<String>,
    source_file: Option<String>,
    source_ordinal: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RowQuery {
    source_file: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationsQuery {
    page_size: Option<u32>,
    cursor: Option<String>,
    namespace: Option<String>,
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionQueueQuery {
    page_size: Option<u32>,
    cursor: Option<String>,
    disposition: Option<String>,
    severity: Option<String>,
    entity_id: Option<String>,
    field_id: Option<String>,
    rule_id: Option<String>,
    source_file: Option<String>,
    reevaluation_state: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReconciliationRowsQuery {
    page_size: Option<u32>,
    cursor: Option<String>,
    category: Option<String>,
    source_file: Option<String>,
    source_ordinal: Option<u64>,
    entity_id: Option<String>,
    issue_id: Option<String>,
    decision_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultActionsQuery {
    page_size: Option<u32>,
    cursor: Option<String>,
    candidate_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct RequiredNullable<T>(Option<T>);

#[derive(Debug)]
struct CommonDecisionCommand {
    schema_version: String,
    validation_run_id: String,
    issue_hash: String,
    practitioner_identifier: String,
    reason: String,
    evidence_reference: Option<String>,
    expected_active_decision_id: Option<String>,
    expected_active_generation: u64,
    acknowledge_not_migration_result_approval: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorrectIssueCommand {
    schema_version: String,
    validation_run_id: String,
    issue_hash: String,
    practitioner_identifier: String,
    reason: String,
    evidence_reference: RequiredNullable<String>,
    expected_active_decision_id: RequiredNullable<String>,
    expected_active_generation: u64,
    acknowledge_not_migration_result_approval: bool,
    correction: CorrectionCommand,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorrectionCommand {
    source_file: String,
    source_ordinal: u64,
    entity_id: String,
    field_id: String,
    operation: CorrectionOperation,
    value: RequiredNullable<String>,
    expected_effective_value_hash: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaiveIssueCommand {
    schema_version: String,
    validation_run_id: String,
    issue_hash: String,
    practitioner_identifier: String,
    reason: String,
    evidence_reference: RequiredNullable<String>,
    expected_active_decision_id: RequiredNullable<String>,
    expected_active_generation: u64,
    acknowledge_not_migration_result_approval: bool,
    acknowledge_waiver_limitations: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptIssueAsIsCommand {
    schema_version: String,
    validation_run_id: String,
    issue_hash: String,
    practitioner_identifier: String,
    reason: String,
    evidence_reference: RequiredNullable<String>,
    expected_active_decision_id: RequiredNullable<String>,
    expected_active_generation: u64,
    acknowledge_not_migration_result_approval: bool,
    expected_effective_value_hash: String,
    acknowledge_validation_truth_unchanged: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExcludeIssueRowCommand {
    schema_version: String,
    validation_run_id: String,
    issue_hash: String,
    practitioner_identifier: String,
    reason: String,
    evidence_reference: RequiredNullable<String>,
    expected_active_decision_id: RequiredNullable<String>,
    expected_active_generation: u64,
    acknowledge_not_migration_result_approval: bool,
    source_file: String,
    source_ordinal: u64,
    acknowledge_row_scope: bool,
}

enum GovernedAction {
    Correct(CorrectIssueCommand),
    Waive(WaiveIssueCommand),
    AcceptAsIs(AcceptIssueAsIsCommand),
    ExcludeRow(ExcludeIssueRowCommand),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalculateReconciliationCommand {
    schema_version: String,
    actor: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateResultCandidateCommand {
    schema_version: String,
    snapshot_id: String,
    snapshot_hash: String,
    creator: String,
    preparation_reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResultActionCommand {
    schema_version: String,
    candidate_id: String,
    candidate_hash: String,
    actor: String,
    reason: String,
    warning_acknowledgments: Vec<String>,
    acknowledge_local_only_boundary: bool,
    expected_active_action_id: RequiredNullable<String>,
    expected_generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupersedeResultApprovalCommand {
    schema_version: String,
    candidate_id: String,
    candidate_hash: String,
    actor: String,
    reason: String,
    warning_acknowledgments: Vec<String>,
    acknowledge_local_only_boundary: bool,
    expected_active_action_id: RequiredNullable<String>,
    expected_generation: u64,
    replacement_candidate_id: String,
    replacement_candidate_hash: String,
}

#[derive(Debug, Serialize)]
struct ResultCommandEnvelope<T> {
    schema_version: &'static str,
    data: T,
    canonical_hash: String,
}

#[derive(Debug, Serialize)]
struct ReconciliationCommandData {
    snapshot_id: String,
    snapshot_hash: String,
    state: &'static str,
    current: bool,
    accepted_row_count: u64,
    blocked_count: u64,
    idempotent_replay: bool,
}

#[derive(Debug, Serialize)]
struct CandidateCommandData {
    candidate_id: String,
    candidate_hash: String,
    snapshot_id: String,
    snapshot_hash: String,
    current: bool,
    eligible: bool,
    generation: u64,
    idempotent_replay: bool,
}

#[derive(Debug, Serialize)]
struct ActionCommandData {
    action_id: String,
    action_kind: String,
    action_hash: String,
    candidate_id: String,
    candidate_hash: String,
    generation: u64,
    active_approval: bool,
    idempotent_replay: bool,
}

struct ActionHead {
    action_id: Option<String>,
    action_hash: Option<String>,
    generation: u64,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    schema_version: &'static str,
    ok: bool,
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    message: &'static str,
    request_id: String,
    retryable: bool,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retryable: bool,
}

impl ApiError {
    const fn new(
        status: StatusCode,
        code: &'static str,
        message: &'static str,
        retryable: bool,
    ) -> Self {
        Self {
            status,
            code,
            message,
            retryable,
        }
    }

    const fn invalid_request() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INVALID_REQUEST",
            "Request parameters are invalid or exceed the supported bounds.",
            false,
        )
    }

    const fn unavailable() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "EVIDENCE_UNAVAILABLE",
            "The requested evidence is unavailable in the startup-selected context.",
            false,
        )
    }

    const fn capability_disabled() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "GOVERNED_DECISIONS_DISABLED",
            "Governed issue decisions are disabled for this local process.",
            false,
        )
    }

    const fn result_approval_disabled() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "RESULT_APPROVAL_DISABLED",
            "Migration-result readiness commands are disabled for this local process.",
            false,
        )
    }

    const fn invalid_governed_transport() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INVALID_GOVERNED_TRANSPORT",
            "The governed command did not satisfy the same-origin transport contract.",
            false,
        )
    }

    const fn invalid_result_transport() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INVALID_RESULT_TRANSPORT",
            "The migration-result command did not satisfy the same-origin transport contract.",
            false,
        )
    }

    const fn invalid_publication_transport() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INVALID_PUBLICATION_TRANSPORT",
            "The readiness-package command did not satisfy the same-origin transport contract.",
            false,
        )
    }

    const fn invalid_publication_command() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INVALID_PUBLICATION_COMMAND",
            "The readiness-package command is invalid or outside the startup-selected job.",
            false,
        )
    }

    const fn invalid_workbench_host() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INVALID_WORKBENCH_HOST",
            "The request Host must identify an explicit loopback workbench authority.",
            false,
        )
    }

    const fn invalid_command() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INVALID_DECISION_COMMAND",
            "The governed decision command is invalid or outside its supported bounds.",
            false,
        )
    }

    const fn command_body_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "DECISION_BODY_BOUND_EXCEEDED",
            "The governed decision command exceeds its request-body bound.",
            false,
        )
    }

    const fn invalid_result_command() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INVALID_RESULT_COMMAND",
            "The migration-result command is invalid or outside its supported bounds.",
            false,
        )
    }

    const fn result_body_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "RESULT_BODY_BOUND_EXCEEDED",
            "The migration-result command exceeds its request-body bound.",
            false,
        )
    }

    fn from_kernel(error: KernelError) -> Self {
        match error {
            KernelError::ReadModel(_) => Self::invalid_request(),
            KernelError::DecisionInvalid(_) => Self::invalid_command(),
            KernelError::ResultApprovalInvalid(_) => Self::invalid_result_command(),
            KernelError::NotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                "EVIDENCE_NOT_FOUND",
                "The requested evidence was not found.",
                false,
            ),
            KernelError::ReadQueryGuard => Self::new(
                StatusCode::GATEWAY_TIMEOUT,
                "QUERY_DEADLINE_EXCEEDED",
                "The bounded read did not complete before its deadline.",
                true,
            ),
            KernelError::Reconciliation(detail) if detail.contains("cursor") => {
                Self::invalid_request()
            }
            KernelError::Conflict(detail) if detail.contains("schema version") => Self::new(
                StatusCode::CONFLICT,
                "SCHEMA_UNSUPPORTED",
                "The workspace schema is unsupported by this workbench.",
                false,
            ),
            KernelError::IntegrityFailed(detail) if detail.contains("schema version") => Self::new(
                StatusCode::CONFLICT,
                "SCHEMA_UNSUPPORTED",
                "The workspace schema is unsupported by this workbench.",
                false,
            ),
            KernelError::DecisionConflict(_) => Self::new(
                StatusCode::CONFLICT,
                "DECISION_CONFLICT",
                "The command conflicts with current governed decision authority.",
                false,
            ),
            KernelError::DecisionIntegrity(_) => Self::new(
                StatusCode::CONFLICT,
                "DECISION_INTEGRITY_FAILED",
                "Governed decision integrity verification failed closed.",
                false,
            ),
            KernelError::ResultApprovalConflict(_) => Self::new(
                StatusCode::CONFLICT,
                "RESULT_APPROVAL_CONFLICT",
                "The command conflicts with current migration-result readiness authority.",
                false,
            ),
            KernelError::ResultApprovalIntegrity(_) => Self::new(
                StatusCode::CONFLICT,
                "RESULT_APPROVAL_INTEGRITY_FAILED",
                "Migration-result readiness integrity verification failed closed.",
                false,
            ),
            KernelError::ResultApprovalBound(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "RESULT_APPROVAL_BOUND_EXCEEDED",
                "The migration-result operation exceeded an explicit safe bound.",
                false,
            ),
            KernelError::DecisionBound(_) | KernelError::ValidationBound(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "DECISION_BOUND_EXCEEDED",
                "The governed decision could not complete within an explicit safe bound.",
                false,
            ),
            KernelError::WorkspaceLocked(_) => Self::new(
                StatusCode::CONFLICT,
                "WORKSPACE_LOCKED",
                "The governed workspace is currently in use; retry the same command identity.",
                true,
            ),
            KernelError::Reconciliation(_)
            | KernelError::IntegrityFailed(_)
            | KernelError::Conflict(_)
            | KernelError::MappingConflict(_)
            | KernelError::ProfileApproval(_)
            | KernelError::ProfileRegistry(_)
            | KernelError::QuarantinedSource(_) => Self::new(
                StatusCode::CONFLICT,
                "INTEGRITY_FAILURE",
                "The requested evidence failed an integrity or authority check.",
                false,
            ),
            KernelError::ProfileNotApproved(_) => Self::new(
                StatusCode::CONFLICT,
                "AUTHORITY_UNAVAILABLE",
                "The required local profile authority is unavailable.",
                false,
            ),
            _ => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "READ_UNAVAILABLE",
                "The bounded read is currently unavailable.",
                false,
            ),
        }
    }

    fn from_read_kernel(error: KernelError) -> Self {
        if matches!(
            &error,
            KernelError::DecisionInvalid(_) | KernelError::ResultApprovalInvalid(_)
        ) {
            Self::invalid_request()
        } else {
            Self::from_kernel(error)
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let body = ErrorEnvelope {
            schema_version: ERROR_SCHEMA,
            ok: false,
            error: ErrorBody {
                code: self.code,
                message: self.message,
                request_id: next_request_id(),
                retryable: self.retryable,
            },
        };
        (self.status, Json(body)).into_response()
    }
}

async fn context(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    bounded_json(state.context, MAXIMUM_BROAD_RESPONSE_BYTES)
}

async fn journey(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    bounded_json(state.journey, MAXIMUM_BROAD_RESPONSE_BYTES)
}

async fn job(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    let job_id = state.context.job_id.clone();
    run_read(
        move || state.reads.job_summary(&job_id),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn files(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<PageQuery>, QueryRejection>,
) -> impl IntoResponse {
    let page = match parse_query(query).and_then(PageQuery::page) {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || state.reads.file_manifest(&job_id, &page),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn profiling(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<ProfilingQuery>, QueryRejection>,
) -> impl IntoResponse {
    let query = match parse_query(query) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    let page = match (PageQuery {
        page_size: query.page_size,
        cursor: query.cursor,
    })
    .page()
    {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let Some(run_id) = state.context.profiling_run_id.clone() else {
        return ApiError::unavailable().into_response();
    };
    let include_samples = query.include_samples;
    run_read(
        move || {
            state
                .reads
                .profiling_summary(&run_id, include_samples, &page)
        },
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn profile(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    let Some(governance) = state.context.selected_governance.clone() else {
        return ApiError::unavailable().into_response();
    };
    run_read(
        move || {
            state.reads.profile_view(
                &governance.profile_id,
                &governance.profile_version,
                &governance.profile_hash,
            )
        },
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn mapping(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<PageQuery>, QueryRejection>,
) -> impl IntoResponse {
    let page = match parse_query(query).and_then(PageQuery::page) {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let Some(governance) = state.context.selected_governance.clone() else {
        return ApiError::unavailable().into_response();
    };
    run_read(
        move || {
            state.reads.mapping_coverage(
                &governance.mapping_id,
                &governance.mapping_version,
                &governance.mapping_hash,
                &page,
            )
        },
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn validation(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    let Some(governance) = state.context.selected_governance.clone() else {
        return ApiError::unavailable().into_response();
    };
    run_read(
        move || {
            state
                .reads
                .validation_summary(&governance.validation_run_id)
        },
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn issues(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<IssueQuery>, QueryRejection>,
) -> impl IntoResponse {
    let query = match parse_query(query) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    let page = match (PageQuery {
        page_size: query.page_size,
        cursor: query.cursor,
    })
    .page()
    {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let filters = IssueFilters {
        severity: query.severity,
        rule_id: query.rule_id,
        entity_id: query.entity_id,
        field_id: query.field_id,
        source_file: query.source_file,
        source_ordinal: query.source_ordinal,
    };
    if let Err(error) = validate_issue_transport(&filters) {
        return error.into_response();
    }
    let Some(governance) = state.context.selected_governance.clone() else {
        return ApiError::unavailable().into_response();
    };
    run_read(
        move || {
            state
                .reads
                .issue_list(&governance.validation_run_id, &filters, &page)
        },
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn decision_queue(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<DecisionQueueQuery>, QueryRejection>,
) -> impl IntoResponse {
    let query = match parse_query(query) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    let page = match (PageQuery {
        page_size: query.page_size,
        cursor: query.cursor,
    })
    .page()
    {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let filters = DecisionQueueFilters {
        disposition: query.disposition,
        severity: query.severity,
        entity_id: query.entity_id,
        field_id: query.field_id,
        rule_id: query.rule_id,
        source_file: query.source_file,
        reevaluation_state: query.reevaluation_state,
    };
    if filters
        .source_file
        .as_deref()
        .is_some_and(|source_file| !source_file_transport_is_safe(source_file))
    {
        return ApiError::invalid_request().into_response();
    }
    let (reads, governance) = match governed_read_scope(&state) {
        Ok(scope) => scope,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.decision_queue(&job_id, &governance.validation_run_id, &filters, &page),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn decision_counts(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    let (reads, governance) = match governed_read_scope(&state) {
        Ok(scope) => scope,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.decision_counts(&job_id, &governance.validation_run_id),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn decision_issue(
    State(state): State<WorkbenchState>,
    issue_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    let issue_id = match bounded_identifier_path(issue_id) {
        Ok(issue_id) => issue_id,
        Err(error) => return error.into_response(),
    };
    let (reads, governance) = match governed_read_scope(&state) {
        Ok(scope) => scope,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.issue_detail(&job_id, &governance.validation_run_id, &issue_id),
        MAXIMUM_ROW_RESPONSE_BYTES,
    )
    .await
}

async fn decision_history(
    State(state): State<WorkbenchState>,
    issue_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    query: std::result::Result<Query<PageQuery>, QueryRejection>,
) -> impl IntoResponse {
    let issue_id = match bounded_identifier_path(issue_id) {
        Ok(issue_id) => issue_id,
        Err(error) => return error.into_response(),
    };
    let page = match parse_query(query).and_then(PageQuery::page) {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let (reads, governance) = match governed_read_scope(&state) {
        Ok(scope) => scope,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.decision_history(&job_id, &governance.validation_run_id, &issue_id, &page),
        MAXIMUM_ROW_RESPONSE_BYTES,
    )
    .await
}

async fn correction_detail(
    State(state): State<WorkbenchState>,
    overlay_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    let overlay_id = match bounded_identifier_path(overlay_id) {
        Ok(overlay_id) => overlay_id,
        Err(error) => return error.into_response(),
    };
    let (reads, _) = match governed_read_scope(&state) {
        Ok(scope) => scope,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.correction_detail_for_job(&job_id, &overlay_id),
        MAXIMUM_ROW_RESPONSE_BYTES,
    )
    .await
}

async fn decision_result(
    State(state): State<WorkbenchState>,
    decision_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    let decision_id = match bounded_identifier_path(decision_id) {
        Ok(decision_id) => decision_id,
        Err(error) => return error.into_response(),
    };
    let (reads, _) = match governed_read_scope(&state) {
        Ok(scope) => scope,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.decision_result_for_job(&job_id, &decision_id),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn result_reconciliation_snapshots(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<PageQuery>, QueryRejection>,
) -> impl IntoResponse {
    let page = match parse_query(query).and_then(PageQuery::page) {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let reads = match result_read_scope(&state) {
        Ok(reads) => reads,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.snapshot_list(&job_id, &page),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn result_reconciliation_snapshot(
    State(state): State<WorkbenchState>,
    snapshot_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    let snapshot_id = match bounded_identifier_path(snapshot_id) {
        Ok(snapshot_id) => snapshot_id,
        Err(error) => return error.into_response(),
    };
    let reads = match result_read_scope(&state) {
        Ok(reads) => reads,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.snapshot_summary(&job_id, &snapshot_id),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn result_reconciliation_rows(
    State(state): State<WorkbenchState>,
    snapshot_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    query: std::result::Result<Query<ReconciliationRowsQuery>, QueryRejection>,
) -> impl IntoResponse {
    let snapshot_id = match bounded_identifier_path(snapshot_id) {
        Ok(snapshot_id) => snapshot_id,
        Err(error) => return error.into_response(),
    };
    let query = match parse_query(query) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    let page = match (PageQuery {
        page_size: query.page_size,
        cursor: query.cursor,
    })
    .page()
    {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let filters = ReconciliationRowFilters {
        category: query.category,
        source_file: query.source_file,
        source_ordinal: query.source_ordinal,
        entity_id: query.entity_id,
        issue_id: query.issue_id,
        decision_id: query.decision_id,
    };
    if let Err(error) = validate_reconciliation_row_transport(&filters) {
        return error.into_response();
    }
    let reads = match result_read_scope(&state) {
        Ok(reads) => reads,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.reconciliation_rows(&job_id, &snapshot_id, &filters, &page),
        MAXIMUM_ROW_RESPONSE_BYTES,
    )
    .await
}

async fn result_reconciliation_row_detail(
    State(state): State<WorkbenchState>,
    path: std::result::Result<AxumPath<(String, u64)>, axum::extract::rejection::PathRejection>,
    query: std::result::Result<Query<RowQuery>, QueryRejection>,
) -> impl IntoResponse {
    let Ok(AxumPath((snapshot_id, source_ordinal))) = path else {
        return ApiError::invalid_request().into_response();
    };
    if !identifier_transport_is_safe(&snapshot_id) || source_ordinal == 0 {
        return ApiError::invalid_request().into_response();
    }
    let query = match parse_query(query) {
        Ok(query) if source_file_transport_is_safe(&query.source_file) => query,
        Ok(_) => return ApiError::invalid_request().into_response(),
        Err(error) => return error.into_response(),
    };
    let reads = match result_read_scope(&state) {
        Ok(reads) => reads,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || {
            reads.reconciliation_row_detail(
                &job_id,
                &snapshot_id,
                &query.source_file,
                source_ordinal,
            )
        },
        MAXIMUM_ROW_RESPONSE_BYTES,
    )
    .await
}

async fn result_candidates(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<PageQuery>, QueryRejection>,
) -> impl IntoResponse {
    let page = match parse_query(query).and_then(PageQuery::page) {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let reads = match result_read_scope(&state) {
        Ok(reads) => reads,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.candidate_list(&job_id, &page),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn result_candidate_eligibility(
    State(state): State<WorkbenchState>,
    candidate_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    let candidate_id = match bounded_identifier_path(candidate_id) {
        Ok(candidate_id) => candidate_id,
        Err(error) => return error.into_response(),
    };
    let reads = match result_read_scope(&state) {
        Ok(reads) => reads,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.approval_eligibility(&job_id, &candidate_id),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn result_actions(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<ResultActionsQuery>, QueryRejection>,
) -> impl IntoResponse {
    let query = match parse_query(query) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    if query
        .candidate_id
        .as_deref()
        .is_some_and(|value| !identifier_transport_is_safe(value))
    {
        return ApiError::invalid_request().into_response();
    }
    let page = match (PageQuery {
        page_size: query.page_size,
        cursor: query.cursor,
    })
    .page()
    {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let reads = match result_read_scope(&state) {
        Ok(reads) => reads,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    let candidate_id = query.candidate_id;
    run_read(
        move || reads.action_history(&job_id, candidate_id.as_deref(), &page),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn calculate_result_reconciliation(
    State(state): State<WorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> axum::response::Response {
    let idempotency_key = match result_command_transport(&state, None, &headers) {
        Ok((None, idempotency_key)) => idempotency_key,
        Ok((Some(_), _)) => return ApiError::invalid_result_command().into_response(),
        Err(error) => return error.into_response(),
    };
    let command = match decode_result_command_body::<CalculateReconciliationCommand>(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != CALCULATE_RECONCILIATION_COMMAND_SCHEMA
        || !valid_result_attribution(&command.actor, &command.reason)
    {
        return ApiError::invalid_result_command().into_response();
    }
    let (_, writes, governance) = match result_write_scope(&state) {
        Ok(scope) => scope,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    let request = CalculateReconciliationRequest {
        schema_version: command.schema_version,
        job_id: job_id.clone(),
        validation_run_id: governance.validation_run_id,
        practitioner_id: command.actor,
        reason: command.reason,
        idempotency_key,
        local_readiness_only_acknowledged: true,
    };
    let result = match run_domain_task(move || writes.calculate(&request)).await {
        Ok(result) => result,
        Err(error) => return error.into_response(),
    };
    let reads = match result_read_scope(&state) {
        Ok(reads) => reads,
        Err(error) => return error.into_response(),
    };
    let snapshot_id = result.snapshot_id.clone();
    let summary = match run_domain_task(move || reads.snapshot_summary(&job_id, &snapshot_id)).await
    {
        Ok(summary) => summary,
        Err(error) => return error.into_response(),
    };
    reconciliation_command_response(result, summary.data.current)
}

async fn create_result_candidate(
    State(state): State<WorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> axum::response::Response {
    let idempotency_key = match result_command_transport(&state, None, &headers) {
        Ok((None, idempotency_key)) => idempotency_key,
        Ok((Some(_), _)) => return ApiError::invalid_result_command().into_response(),
        Err(error) => return error.into_response(),
    };
    let command = match decode_result_command_body::<CreateResultCandidateCommand>(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != CREATE_RESULT_CANDIDATE_COMMAND_SCHEMA
        || !identifier_transport_is_safe(&command.snapshot_id)
        || !sha256_transport_is_safe(&command.snapshot_hash)
        || !valid_result_attribution(&command.creator, &command.preparation_reason)
    {
        return ApiError::invalid_result_command().into_response();
    }
    let (_, writes, _) = match result_write_scope(&state) {
        Ok(scope) => scope,
        Err(error) => return error.into_response(),
    };
    let request = CreateCandidateRequest {
        schema_version: command.schema_version,
        job_id: state.context.job_id.clone(),
        snapshot_id: command.snapshot_id,
        snapshot_hash: command.snapshot_hash,
        practitioner_id: command.creator,
        reason: command.preparation_reason,
        idempotency_key,
        local_readiness_only_acknowledged: true,
    };
    match run_domain_task(move || writes.create_candidate(&request)).await {
        Ok(result) => candidate_command_response(result),
        Err(error) => error.into_response(),
    }
}

async fn approve_result_candidate(
    State(state): State<WorkbenchState>,
    candidate_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    headers: HeaderMap,
    body: Body,
) -> axum::response::Response {
    apply_result_action(
        state,
        candidate_id,
        headers,
        body,
        ResultActionKind::Approved,
        APPROVE_RESULT_CANDIDATE_COMMAND_SCHEMA,
    )
    .await
}

async fn reject_result_candidate(
    State(state): State<WorkbenchState>,
    candidate_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    headers: HeaderMap,
    body: Body,
) -> axum::response::Response {
    apply_result_action(
        state,
        candidate_id,
        headers,
        body,
        ResultActionKind::Rejected,
        REJECT_RESULT_CANDIDATE_COMMAND_SCHEMA,
    )
    .await
}

async fn revoke_result_approval(
    State(state): State<WorkbenchState>,
    candidate_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    headers: HeaderMap,
    body: Body,
) -> axum::response::Response {
    apply_result_action(
        state,
        candidate_id,
        headers,
        body,
        ResultActionKind::ApprovalRevoked,
        REVOKE_RESULT_APPROVAL_COMMAND_SCHEMA,
    )
    .await
}

#[allow(clippy::too_many_lines)]
async fn supersede_result_approval(
    State(state): State<WorkbenchState>,
    candidate_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    headers: HeaderMap,
    body: Body,
) -> axum::response::Response {
    let (candidate_id, idempotency_key) =
        match result_command_transport(&state, Some(candidate_id), &headers) {
            Ok((Some(candidate_id), idempotency_key)) => (candidate_id, idempotency_key),
            Ok((None, _)) => return ApiError::invalid_result_command().into_response(),
            Err(error) => return error.into_response(),
        };
    let command = match decode_result_command_body::<SupersedeResultApprovalCommand>(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != SUPERSEDE_RESULT_APPROVAL_COMMAND_SCHEMA
        || command.candidate_id != candidate_id
        || command.replacement_candidate_id == candidate_id
        || !identifier_transport_is_safe(&command.replacement_candidate_id)
        || !sha256_transport_is_safe(&command.replacement_candidate_hash)
        || !valid_result_action_fields(
            &command.candidate_hash,
            &command.actor,
            &command.reason,
            &command.warning_acknowledgments,
            command.acknowledge_local_only_boundary,
        )
    {
        return ApiError::invalid_result_command().into_response();
    }
    let (reads, writes, _) = match result_write_scope(&state) {
        Ok(scope) => scope,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    let replay_probe = ResultActionReplayProbe {
        kind: ResultActionKind::ApprovalSuperseded,
        candidate_id: command.replacement_candidate_id.clone(),
        candidate_hash: command.replacement_candidate_hash.clone(),
        actor: command.actor.clone(),
        reason: command.reason.clone(),
        warning_acknowledgements: command.warning_acknowledgments.clone(),
        authority_boundary_acknowledged: command.acknowledge_local_only_boundary,
        expected_predecessor_action_id: command.expected_active_action_id.0.clone(),
        expected_generation: command.expected_generation,
        idempotency_key: idempotency_key.clone(),
        superseded_candidate_id: Some(candidate_id.clone()),
        superseded_candidate_hash: Some(command.candidate_hash.clone()),
    };
    match exact_result_action_replay(&writes, &job_id, &replay_probe).await {
        Ok(Some(result)) => return action_command_response(result),
        Ok(None) => {}
        Err(error) => return error.into_response(),
    }
    let current_id = candidate_id.clone();
    let replacement_id = command.replacement_candidate_id.clone();
    let read_job_id = job_id.clone();
    let evidence = run_domain_task(move || {
        let current = reads.approval_eligibility(&read_job_id, &current_id)?;
        let replacement = reads.approval_eligibility(&read_job_id, &replacement_id)?;
        let history = reads.action_history(
            &read_job_id,
            None,
            &PageRequest {
                page_size: 1,
                cursor: None,
            },
        )?;
        Ok((current, replacement, history))
    })
    .await;
    let (current, replacement, history) = match evidence {
        Ok(evidence) => evidence,
        Err(error) => return error.into_response(),
    };
    if command.candidate_hash != current.data.candidate_hash
        || command.replacement_candidate_hash != replacement.data.candidate_hash
        || current.data.expected_active_action_id.as_deref()
            != command.expected_active_action_id.0.as_deref()
        || current.data.expected_generation != command.expected_generation
    {
        return replay_or_result_conflict(&writes, &job_id, &replay_probe).await;
    }
    let Ok(head) = action_head_from_history(&history.items, command.expected_generation) else {
        return replay_or_result_conflict(&writes, &job_id, &replay_probe).await;
    };
    if head.action_id.as_deref() != command.expected_active_action_id.0.as_deref()
        || history
            .items
            .first()
            .is_none_or(|item| item.candidate_id != candidate_id)
    {
        return replay_or_result_conflict(&writes, &job_id, &replay_probe).await;
    }
    let payload = ResultActionPayload {
        schema_version: RECONCILIATION_SCHEMA_VERSION,
        kind: ResultActionKind::ApprovalSuperseded,
        candidate_id: replacement.data.candidate_id,
        candidate_hash: replacement.data.candidate_hash,
        snapshot_id: replacement.data.snapshot_id,
        snapshot_hash: replacement.data.snapshot_hash,
        actor: command.actor,
        reason: command.reason,
        warning_acknowledgements: command.warning_acknowledgments,
        authority_boundary_acknowledged: true,
        authority_boundary_acknowledgement_version: RESULT_BOUNDARY_ACKNOWLEDGEMENT_VERSION
            .to_owned(),
        predecessor_action_id: head.action_id,
        predecessor_action_hash: head.action_hash,
        observed_generation: head.generation,
        idempotency_key,
    };
    let action_writes = writes.clone();
    let action_job_id = job_id.clone();
    match run_domain_task(move || action_writes.apply_action(&action_job_id, &payload)).await {
        Ok(result) => action_command_response(result),
        Err(error) if error.code == "RESULT_APPROVAL_CONFLICT" => {
            replay_or_result_conflict(&writes, &job_id, &replay_probe).await
        }
        Err(error) => error.into_response(),
    }
}

fn publication_read_scope(
    state: &WorkbenchState,
) -> std::result::Result<crate::publication_read_model::PublicationReadService, ApiError> {
    state
        .publication_reads
        .clone()
        .ok_or_else(publication_capability_disabled)
}

fn publication_write_scope(
    state: &WorkbenchState,
) -> std::result::Result<crate::publication_service::PublicationService, ApiError> {
    state
        .publication_writes
        .clone()
        .ok_or_else(publication_capability_disabled)
}

fn publication_capability_disabled() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "PUBLICATION_CAPABILITY_DISABLED",
        "Readiness publication is not enabled for this workbench process.",
        false,
    )
}

async fn publication_eligibility(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    let reads = match publication_read_scope(&state) {
        Ok(reads) => reads,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.get_eligibility(&job_id),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn list_publications(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    let reads = match publication_read_scope(&state) {
        Ok(reads) => reads,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || reads.list_publications(&job_id),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn get_publication_detail(
    State(state): State<WorkbenchState>,
    publication_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    query: std::result::Result<Query<EmptyQuery>, QueryRejection>,
) -> impl IntoResponse {
    if let Err(error) = parse_query(query) {
        return error.into_response();
    }
    let pub_id = match bounded_identifier_path(publication_id) {
        Ok(pub_id) => pub_id,
        Err(error) => return error.into_response(),
    };
    let reads = match publication_read_scope(&state) {
        Ok(reads) => reads,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    run_read(
        move || {
            reads
                .get_detail_for_job(&job_id, &pub_id)?
                .ok_or_else(|| KernelError::NotFound(format!("Publication {pub_id} not found")))
        },
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn create_publication(
    State(state): State<WorkbenchState>,
    headers: HeaderMap,
    request: std::result::Result<
        Json<crate::publication::PublicationRequest>,
        axum::extract::rejection::JsonRejection,
    >,
) -> impl IntoResponse {
    let Ok(Json(req)) = request else {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "INVALID_JSON",
            "Publication request JSON is invalid",
            false,
        )
        .into_response();
    };
    let idempotency_key = match validate_publication_headers(&headers) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if req.schema_version != PUBLICATION_COMMAND_SCHEMA
        || req.job_id != state.context.job_id
        || req.idempotency_key != idempotency_key
    {
        return ApiError::invalid_publication_command().into_response();
    }

    let service = match publication_write_scope(&state) {
        Ok(service) => service,
        Err(error) => return error.into_response(),
    };

    let maximum_bytes = MAXIMUM_BROAD_RESPONSE_BYTES;
    match tokio::time::timeout(
        TASK_TIMEOUT,
        tokio::task::spawn_blocking(move || service.publish(&req)),
    )
    .await
    {
        Ok(Ok(Ok(value))) => bounded_json(value, maximum_bytes),
        Ok(Ok(Err(error))) => ApiError::from_read_kernel(error).into_response(),
        Ok(Err(_)) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "WRITE_TASK_FAILED",
            "The publication write task did not complete safely.",
            false,
        )
        .into_response(),
        Err(_) => ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "QUERY_DEADLINE_EXCEEDED",
            "The publication write did not complete before its deadline.",
            true,
        )
        .into_response(),
    }
}

async fn download_publication(
    State(state): State<WorkbenchState>,
    publication_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
) -> impl IntoResponse {
    let pub_id = match bounded_identifier_path(publication_id) {
        Ok(pub_id) => pub_id,
        Err(error) => return error.into_response(),
    };
    let reads = match publication_read_scope(&state) {
        Ok(reads) => reads,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    let record = match reads.get_detail_for_job(&job_id, &pub_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            return ApiError::new(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "Publication not found",
                false,
            )
            .into_response();
        }
        Err(e) => return ApiError::from_read_kernel(e).into_response(),
    };

    let zip_path = state
        .identity
        .workspace_path
        .join(&record.archive_relative_path);
    let bytes = match fs::read(&zip_path) {
        Ok(b) => b,
        Err(e) => {
            return ApiError::from_read_kernel(KernelError::Io {
                path: zip_path,
                source: e,
            })
            .into_response();
        }
    };

    let filename = format!("wp007-readiness-{}-{}.zip", record.job_id, pub_id);
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    if let Ok(disposition) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
    {
        response
            .headers_mut()
            .insert(axum::http::header::CONTENT_DISPOSITION, disposition);
    }
    response
}

async fn correct_issue(
    State(state): State<WorkbenchState>,
    issue_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    headers: HeaderMap,
    body: Body,
) -> axum::response::Response {
    let (issue_id, idempotency_key) = match governed_command_transport(&state, issue_id, &headers) {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    let command = match decode_command_body::<CorrectIssueCommand>(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    apply_governed_action(
        state,
        issue_id,
        idempotency_key,
        GovernedAction::Correct(command),
    )
    .await
}

async fn waive_issue(
    State(state): State<WorkbenchState>,
    issue_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    headers: HeaderMap,
    body: Body,
) -> axum::response::Response {
    let (issue_id, idempotency_key) = match governed_command_transport(&state, issue_id, &headers) {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    let command = match decode_command_body::<WaiveIssueCommand>(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    apply_governed_action(
        state,
        issue_id,
        idempotency_key,
        GovernedAction::Waive(command),
    )
    .await
}

async fn accept_issue_as_is(
    State(state): State<WorkbenchState>,
    issue_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    headers: HeaderMap,
    body: Body,
) -> axum::response::Response {
    let (issue_id, idempotency_key) = match governed_command_transport(&state, issue_id, &headers) {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    let command = match decode_command_body::<AcceptIssueAsIsCommand>(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    apply_governed_action(
        state,
        issue_id,
        idempotency_key,
        GovernedAction::AcceptAsIs(command),
    )
    .await
}

async fn exclude_issue_row(
    State(state): State<WorkbenchState>,
    issue_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    headers: HeaderMap,
    body: Body,
) -> axum::response::Response {
    let (issue_id, idempotency_key) = match governed_command_transport(&state, issue_id, &headers) {
        Ok(context) => context,
        Err(error) => return error.into_response(),
    };
    let command = match decode_command_body::<ExcludeIssueRowCommand>(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    apply_governed_action(
        state,
        issue_id,
        idempotency_key,
        GovernedAction::ExcludeRow(command),
    )
    .await
}

async fn apply_governed_action(
    state: WorkbenchState,
    issue_id: String,
    idempotency_key: String,
    action: GovernedAction,
) -> axum::response::Response {
    let (reads, writes, governance) = match governed_write_scope(&state) {
        Ok(scope) => scope,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    let validation_run_id = governance.validation_run_id.clone();
    let issue_for_read = issue_id.clone();
    let detail = match run_domain_task(move || {
        reads.issue_detail(&job_id, &validation_run_id, &issue_for_read)
    })
    .await
    {
        Ok(detail) => detail,
        Err(error) => return error.into_response(),
    };
    let request =
        match derive_decision_request(&issue_id, idempotency_key, action, &governance, &detail) {
            Ok(request) => request,
            Err(error) => return error.into_response(),
        };
    match run_domain_task(move || writes.apply(&request)).await {
        Ok(result) => bounded_json(result, MAXIMUM_BROAD_RESPONSE_BYTES),
        Err(error) => error.into_response(),
    }
}

#[allow(clippy::too_many_lines)]
async fn apply_result_action(
    state: WorkbenchState,
    candidate_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    headers: HeaderMap,
    body: Body,
    kind: ResultActionKind,
    expected_schema: &'static str,
) -> axum::response::Response {
    let (candidate_id, idempotency_key) =
        match result_command_transport(&state, Some(candidate_id), &headers) {
            Ok((Some(candidate_id), idempotency_key)) => (candidate_id, idempotency_key),
            Ok((None, _)) => return ApiError::invalid_result_command().into_response(),
            Err(error) => return error.into_response(),
        };
    let command = match decode_result_command_body::<ResultActionCommand>(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != expected_schema
        || command.candidate_id != candidate_id
        || !valid_result_action_fields(
            &command.candidate_hash,
            &command.actor,
            &command.reason,
            &command.warning_acknowledgments,
            command.acknowledge_local_only_boundary,
        )
    {
        return ApiError::invalid_result_command().into_response();
    }
    let (reads, writes, _) = match result_write_scope(&state) {
        Ok(scope) => scope,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    let replay_probe = ResultActionReplayProbe {
        kind,
        candidate_id: candidate_id.clone(),
        candidate_hash: command.candidate_hash.clone(),
        actor: command.actor.clone(),
        reason: command.reason.clone(),
        warning_acknowledgements: command.warning_acknowledgments.clone(),
        authority_boundary_acknowledged: command.acknowledge_local_only_boundary,
        expected_predecessor_action_id: command.expected_active_action_id.0.clone(),
        expected_generation: command.expected_generation,
        idempotency_key: idempotency_key.clone(),
        superseded_candidate_id: None,
        superseded_candidate_hash: None,
    };
    match exact_result_action_replay(&writes, &job_id, &replay_probe).await {
        Ok(Some(result)) => return action_command_response(result),
        Ok(None) => {}
        Err(error) => return error.into_response(),
    }
    let candidate_for_read = candidate_id.clone();
    let read_job_id = job_id.clone();
    let evidence = run_domain_task(move || {
        let eligibility = reads.approval_eligibility(&read_job_id, &candidate_for_read)?;
        let history = reads.action_history(
            &read_job_id,
            None,
            &PageRequest {
                page_size: 1,
                cursor: None,
            },
        )?;
        Ok((eligibility, history))
    })
    .await;
    let (eligibility, history) = match evidence {
        Ok(evidence) => evidence,
        Err(error) => return error.into_response(),
    };
    if command.candidate_hash != eligibility.data.candidate_hash
        || eligibility.data.expected_active_action_id.as_deref()
            != command.expected_active_action_id.0.as_deref()
        || eligibility.data.expected_generation != command.expected_generation
    {
        return replay_or_result_conflict(&writes, &job_id, &replay_probe).await;
    }
    let Ok(head) = action_head_from_history(&history.items, command.expected_generation) else {
        return replay_or_result_conflict(&writes, &job_id, &replay_probe).await;
    };
    if head.action_id.as_deref() != command.expected_active_action_id.0.as_deref() {
        return replay_or_result_conflict(&writes, &job_id, &replay_probe).await;
    }
    let payload = ResultActionPayload {
        schema_version: RECONCILIATION_SCHEMA_VERSION,
        kind,
        candidate_id: eligibility.data.candidate_id,
        candidate_hash: eligibility.data.candidate_hash,
        snapshot_id: eligibility.data.snapshot_id,
        snapshot_hash: eligibility.data.snapshot_hash,
        actor: command.actor,
        reason: command.reason,
        warning_acknowledgements: command.warning_acknowledgments,
        authority_boundary_acknowledged: true,
        authority_boundary_acknowledgement_version: RESULT_BOUNDARY_ACKNOWLEDGEMENT_VERSION
            .to_owned(),
        predecessor_action_id: head.action_id,
        predecessor_action_hash: head.action_hash,
        observed_generation: head.generation,
        idempotency_key,
    };
    let action_writes = writes.clone();
    let action_job_id = job_id.clone();
    match run_domain_task(move || action_writes.apply_action(&action_job_id, &payload)).await {
        Ok(result) => action_command_response(result),
        Err(error) if error.code == "RESULT_APPROVAL_CONFLICT" => {
            replay_or_result_conflict(&writes, &job_id, &replay_probe).await
        }
        Err(error) => error.into_response(),
    }
}

fn action_head_from_history(
    items: &[ResultActionSummary],
    expected_generation: u64,
) -> std::result::Result<ActionHead, ApiError> {
    match (expected_generation, items.first()) {
        (0, None) => Ok(ActionHead {
            action_id: None,
            action_hash: None,
            generation: 0,
        }),
        (generation, Some(item)) if item.generation == generation => Ok(ActionHead {
            action_id: Some(item.action_id.clone()),
            action_hash: Some(item.action_hash.clone()),
            generation,
        }),
        _ => Err(result_conflict()),
    }
}

fn reconciliation_command_response(
    result: ReconciliationCommandResult,
    current: bool,
) -> axum::response::Response {
    let data = ReconciliationCommandData {
        snapshot_id: result.snapshot_id,
        snapshot_hash: result.snapshot_hash,
        state: "COMPLETE",
        current,
        accepted_row_count: result.accepted_row_count,
        blocked_count: result.blocked_count,
        idempotent_replay: result.idempotent_replay,
    };
    result_command_envelope(RECONCILIATION_COMMAND_RESULT_SCHEMA, data)
}

fn candidate_command_response(result: CandidateCommandResult) -> axum::response::Response {
    let data = CandidateCommandData {
        candidate_id: result.candidate_id,
        candidate_hash: result.candidate_hash,
        snapshot_id: result.snapshot_id,
        snapshot_hash: result.snapshot_hash,
        current: result.current,
        eligible: result.eligible,
        generation: result.generation,
        idempotent_replay: result.idempotent_replay,
    };
    result_command_envelope(CANDIDATE_COMMAND_RESULT_SCHEMA, data)
}

fn action_command_response(result: ResultActionCommandResult) -> axum::response::Response {
    let data = ActionCommandData {
        action_id: result.action_id,
        action_kind: result.action_kind,
        action_hash: result.action_hash,
        candidate_id: result.candidate_id,
        candidate_hash: result.candidate_hash,
        generation: result.generation,
        active_approval: result.active,
        idempotent_replay: result.idempotent_replay,
    };
    result_command_envelope(ACTION_COMMAND_RESULT_SCHEMA, data)
}

async fn exact_result_action_replay(
    writes: &ReconciliationApprovalService,
    job_id: &str,
    probe: &ResultActionReplayProbe,
) -> std::result::Result<Option<ResultActionCommandResult>, ApiError> {
    let writes = writes.clone();
    let job_id = job_id.to_owned();
    let probe = probe.clone();
    run_domain_task(move || writes.replay_action_if_exact(&job_id, &probe)).await
}

async fn replay_or_result_conflict(
    writes: &ReconciliationApprovalService,
    job_id: &str,
    probe: &ResultActionReplayProbe,
) -> axum::response::Response {
    match exact_result_action_replay(writes, job_id, probe).await {
        Ok(Some(result)) => action_command_response(result),
        Ok(None) => result_conflict().into_response(),
        Err(error) => error.into_response(),
    }
}

fn result_command_envelope<T: Serialize>(
    schema_version: &'static str,
    data: T,
) -> axum::response::Response {
    let mut response = ResultCommandEnvelope {
        schema_version,
        data,
        canonical_hash: String::new(),
    };
    let hash = serde_json::to_value(&response)
        .map_err(KernelError::from)
        .and_then(|value| canonical_hash(&value));
    match hash {
        Ok(hash) => {
            response.canonical_hash = hash;
            bounded_json(response, MAXIMUM_BROAD_RESPONSE_BYTES)
        }
        Err(error) => ApiError::from_kernel(error).into_response(),
    }
}

fn result_conflict() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "RESULT_APPROVAL_CONFLICT",
        "The command conflicts with current migration-result readiness authority.",
        false,
    )
}

async fn decode_command_body<T>(body: Body) -> std::result::Result<T, ApiError>
where
    T: for<'de> Deserialize<'de>,
{
    let bytes = to_bytes(body, MAXIMUM_COMMAND_BODY_BYTES)
        .await
        .map_err(|_| ApiError::command_body_too_large())?;
    if bytes.is_empty() {
        return Err(ApiError::invalid_command());
    }
    serde_json::from_slice(&bytes).map_err(|_| ApiError::invalid_command())
}

async fn decode_result_command_body<T>(body: Body) -> std::result::Result<T, ApiError>
where
    T: for<'de> Deserialize<'de>,
{
    let bytes = to_bytes(body, MAXIMUM_COMMAND_BODY_BYTES)
        .await
        .map_err(|_| ApiError::result_body_too_large())?;
    if bytes.is_empty() {
        return Err(ApiError::invalid_result_command());
    }
    serde_json::from_slice(&bytes).map_err(|_| ApiError::invalid_result_command())
}

async fn run_domain_task<T, F>(operation: F) -> std::result::Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(ApiError::from_kernel(error)),
        Err(_) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "DECISION_TASK_FAILED",
            "The governed decision task did not complete safely.",
            false,
        )),
    }
}

async fn row_detail(
    State(state): State<WorkbenchState>,
    source_ordinal: std::result::Result<AxumPath<u64>, axum::extract::rejection::PathRejection>,
    query: std::result::Result<Query<RowQuery>, QueryRejection>,
) -> impl IntoResponse {
    let source_ordinal = match source_ordinal {
        Ok(AxumPath(source_ordinal)) if source_ordinal > 0 => source_ordinal,
        _ => return ApiError::invalid_request().into_response(),
    };
    let query = match parse_query(query) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    if query.source_file.is_empty() || query.source_file.len() > MAXIMUM_FILTER_BYTES {
        return ApiError::invalid_request().into_response();
    }
    if !source_file_transport_is_safe(&query.source_file) {
        return ApiError::invalid_request().into_response();
    }
    let Some(governance) = state.context.selected_governance.clone() else {
        return ApiError::unavailable().into_response();
    };
    run_read(
        move || {
            state.reads.row_detail(
                &governance.validation_run_id,
                &query.source_file,
                source_ordinal,
            )
        },
        MAXIMUM_ROW_RESPONSE_BYTES,
    )
    .await
}

async fn reconciliation(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<PageQuery>, QueryRejection>,
) -> impl IntoResponse {
    if !state.reconciliation_summary_available {
        return ApiError::result_approval_disabled().into_response();
    }
    let page = match parse_query(query).and_then(PageQuery::page) {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let job_id = state.context.job_id.clone();
    let validation_run_id = state
        .context
        .selected_governance
        .as_ref()
        .map(|governance| governance.validation_run_id.clone());
    run_read(
        move || {
            state
                .reads
                .reconciliation_summary(&job_id, validation_run_id.as_deref(), &page)
        },
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn operations(
    State(state): State<WorkbenchState>,
    query: std::result::Result<Query<OperationsQuery>, QueryRejection>,
) -> impl IntoResponse {
    let query = match parse_query(query) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    let page = match (PageQuery {
        page_size: query.page_size,
        cursor: query.cursor,
    })
    .page()
    {
        Ok(page) => page,
        Err(error) => return error.into_response(),
    };
    let filters = CleanupFilters {
        namespace: query.namespace,
        state: query.state,
    };
    run_read(
        move || state.reads.operational_cleanup(&filters, &page),
        MAXIMUM_BROAD_RESPONSE_BYTES,
    )
    .await
}

async fn run_read<T, F>(operation: F, maximum_bytes: usize) -> axum::response::Response
where
    T: Serialize + Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    match tokio::time::timeout(TASK_TIMEOUT, tokio::task::spawn_blocking(operation)).await {
        Ok(Ok(Ok(value))) => bounded_json(value, maximum_bytes),
        Ok(Ok(Err(error))) => ApiError::from_read_kernel(error).into_response(),
        Ok(Err(_)) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "READ_TASK_FAILED",
            "The bounded read task did not complete safely.",
            false,
        )
        .into_response(),
        Err(_) => ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "QUERY_DEADLINE_EXCEEDED",
            "The bounded read did not complete before its deadline.",
            true,
        )
        .into_response(),
    }
}

fn bounded_json<T: Serialize>(value: T, maximum_bytes: usize) -> axum::response::Response {
    match serde_json::to_vec(&value) {
        Ok(bytes) if bytes.len() <= maximum_bytes => {
            let mut response = Response::new(Body::from(bytes));
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            response
        }
        Ok(_) => ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "RESPONSE_BOUND_EXCEEDED",
            "The requested response exceeds the workbench delivery bound.",
            false,
        )
        .into_response(),
        Err(_) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "RESPONSE_ENCODING_FAILED",
            "The bounded response could not be encoded safely.",
            false,
        )
        .into_response(),
    }
}

fn parse_query<T>(
    query: std::result::Result<Query<T>, QueryRejection>,
) -> std::result::Result<T, ApiError> {
    query
        .map(|Query(value)| value)
        .map_err(|_| ApiError::invalid_request())
}

fn governed_read_scope(
    state: &WorkbenchState,
) -> std::result::Result<(DecisionReadService, SelectedGovernance), ApiError> {
    let reads = state
        .decision_reads
        .clone()
        .ok_or_else(ApiError::capability_disabled)?;
    let governance = state
        .context
        .selected_governance
        .clone()
        .ok_or_else(ApiError::unavailable)?;
    Ok((reads, governance))
}

fn governed_write_scope(
    state: &WorkbenchState,
) -> std::result::Result<(DecisionReadService, DecisionService, SelectedGovernance), ApiError> {
    let (reads, governance) = governed_read_scope(state)?;
    let writes = state
        .decision_writes
        .clone()
        .ok_or_else(ApiError::capability_disabled)?;
    Ok((reads, writes, governance))
}

fn result_read_scope(
    state: &WorkbenchState,
) -> std::result::Result<ReconciliationReadService, ApiError> {
    state
        .reconciliation_reads
        .clone()
        .ok_or_else(ApiError::result_approval_disabled)
}

fn result_write_scope(
    state: &WorkbenchState,
) -> std::result::Result<
    (
        ReconciliationReadService,
        ReconciliationApprovalService,
        SelectedGovernance,
    ),
    ApiError,
> {
    let reads = result_read_scope(state)?;
    let writes = state
        .reconciliation_writes
        .clone()
        .ok_or_else(ApiError::result_approval_disabled)?;
    let governance = state
        .context
        .selected_governance
        .clone()
        .ok_or_else(ApiError::unavailable)?;
    Ok((reads, writes, governance))
}

fn result_command_transport(
    state: &WorkbenchState,
    candidate_id: Option<
        std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    >,
    headers: &HeaderMap,
) -> std::result::Result<(Option<String>, String), ApiError> {
    if state.reconciliation_writes.is_none() {
        return Err(ApiError::result_approval_disabled());
    }
    let candidate_id = candidate_id.map(bounded_identifier_path).transpose()?;
    let idempotency_key = validate_result_headers(headers)?;
    Ok((candidate_id, idempotency_key))
}

fn validate_result_headers(headers: &HeaderMap) -> std::result::Result<String, ApiError> {
    if headers.contains_key("forwarded")
        || headers
            .keys()
            .any(|name| name.as_str().starts_with("x-forwarded-"))
    {
        return Err(ApiError::invalid_result_transport());
    }
    let host =
        validate_loopback_host_header(headers).map_err(|_| ApiError::invalid_result_transport())?;
    let origin = single_result_header(headers, ORIGIN.as_str())?;
    if origin != format!("http://{host}") {
        return Err(ApiError::invalid_result_transport());
    }
    let content_type = single_result_header(headers, CONTENT_TYPE.as_str())?;
    if !content_type.eq_ignore_ascii_case("application/json") {
        return Err(ApiError::invalid_result_transport());
    }
    let intent = single_result_header(headers, "x-migration-stream-intent")?;
    if intent != RESULT_APPROVAL_INTENT {
        return Err(ApiError::invalid_result_transport());
    }
    let idempotency_key = single_result_header(headers, "idempotency-key")?;
    if !idempotency_key_transport_is_safe(idempotency_key) {
        return Err(ApiError::invalid_result_transport());
    }
    if let Some(length) = optional_single_result_header(headers, CONTENT_LENGTH.as_str())? {
        let length = length
            .parse::<usize>()
            .map_err(|_| ApiError::invalid_result_transport())?;
        if length == 0 || length > MAXIMUM_COMMAND_BODY_BYTES {
            return Err(ApiError::result_body_too_large());
        }
    }
    Ok(idempotency_key.to_owned())
}

fn validate_publication_headers(headers: &HeaderMap) -> std::result::Result<String, ApiError> {
    if headers.contains_key("forwarded")
        || headers
            .keys()
            .any(|name| name.as_str().starts_with("x-forwarded-"))
    {
        return Err(ApiError::invalid_publication_transport());
    }
    let host = validate_loopback_host_header(headers)
        .map_err(|_| ApiError::invalid_publication_transport())?;
    let origin = single_publication_header(headers, ORIGIN.as_str())?;
    if origin != format!("http://{host}") {
        return Err(ApiError::invalid_publication_transport());
    }
    let content_type = single_publication_header(headers, CONTENT_TYPE.as_str())?;
    if !content_type.eq_ignore_ascii_case("application/json") {
        return Err(ApiError::invalid_publication_transport());
    }
    let intent = single_publication_header(headers, "x-migration-stream-intent")?;
    if intent != READINESS_PUBLICATION_INTENT {
        return Err(ApiError::invalid_publication_transport());
    }
    let idempotency_key = single_publication_header(headers, "idempotency-key")?;
    if !idempotency_key_transport_is_safe(idempotency_key) {
        return Err(ApiError::invalid_publication_transport());
    }
    if let Some(length) = optional_single_publication_header(headers, CONTENT_LENGTH.as_str())? {
        let length = length
            .parse::<usize>()
            .map_err(|_| ApiError::invalid_publication_transport())?;
        if length == 0 || length > MAXIMUM_COMMAND_BODY_BYTES {
            return Err(ApiError::invalid_publication_transport());
        }
    }
    Ok(idempotency_key.to_owned())
}

fn single_publication_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> std::result::Result<&'a str, ApiError> {
    optional_single_publication_header(headers, name)?
        .ok_or_else(ApiError::invalid_publication_transport)
}

fn optional_single_publication_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> std::result::Result<Option<&'a str>, ApiError> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(ApiError::invalid_publication_transport());
    }
    value
        .to_str()
        .map(Some)
        .map_err(|_| ApiError::invalid_publication_transport())
}

fn single_result_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> std::result::Result<&'a str, ApiError> {
    optional_single_result_header(headers, name)?.ok_or_else(ApiError::invalid_result_transport)
}

fn optional_single_result_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> std::result::Result<Option<&'a str>, ApiError> {
    let mut values = headers.get_all(name).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(ApiError::invalid_result_transport());
    }
    value
        .map(|value| {
            value
                .to_str()
                .map_err(|_| ApiError::invalid_result_transport())
        })
        .transpose()
}

fn governed_command_transport(
    state: &WorkbenchState,
    issue_id: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    headers: &HeaderMap,
) -> std::result::Result<(String, String), ApiError> {
    if state.decision_writes.is_none() {
        return Err(ApiError::capability_disabled());
    }
    let issue_id = bounded_identifier_path(issue_id)?;
    let idempotency_key = validate_governed_headers(headers)?;
    Ok((issue_id, idempotency_key))
}

fn validate_governed_headers(headers: &HeaderMap) -> std::result::Result<String, ApiError> {
    if headers.contains_key("forwarded")
        || headers
            .keys()
            .any(|name| name.as_str().starts_with("x-forwarded-"))
    {
        return Err(ApiError::invalid_governed_transport());
    }
    let host = validate_loopback_host_header(headers)?;
    let origin = single_header(headers, ORIGIN.as_str())?;
    if origin != format!("http://{host}") {
        return Err(ApiError::invalid_governed_transport());
    }
    let content_type = single_header(headers, CONTENT_TYPE.as_str())?;
    if !content_type.eq_ignore_ascii_case("application/json") {
        return Err(ApiError::invalid_governed_transport());
    }
    let intent = single_header(headers, "x-migration-stream-intent")?;
    if intent != GOVERNED_INTENT {
        return Err(ApiError::invalid_governed_transport());
    }
    let idempotency_key = single_header(headers, "idempotency-key")?;
    if idempotency_key.len() < MINIMUM_IDEMPOTENCY_KEY_BYTES
        || idempotency_key.len() > MAXIMUM_IDEMPOTENCY_KEY_BYTES
        || !idempotency_key.as_bytes()[0].is_ascii_alphanumeric()
        || !idempotency_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(ApiError::invalid_governed_transport());
    }
    if let Some(length) = optional_single_header(headers, CONTENT_LENGTH.as_str())? {
        let length = length
            .parse::<usize>()
            .map_err(|_| ApiError::invalid_governed_transport())?;
        if length == 0 || length > MAXIMUM_COMMAND_BODY_BYTES {
            return Err(ApiError::command_body_too_large());
        }
    }
    Ok(idempotency_key.to_owned())
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> std::result::Result<&'a str, ApiError> {
    optional_single_header(headers, name)?.ok_or_else(ApiError::invalid_governed_transport)
}

fn optional_single_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> std::result::Result<Option<&'a str>, ApiError> {
    let mut values = headers.get_all(name).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err(ApiError::invalid_governed_transport());
    }
    value
        .map(|value| {
            value
                .to_str()
                .map_err(|_| ApiError::invalid_governed_transport())
        })
        .transpose()
}

fn host_is_loopback(host: &str) -> bool {
    let address = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || address
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn authority_port_is_valid(authority: &axum::http::uri::Authority) -> bool {
    let suffix = &authority.as_str()[authority.host().len()..];
    if suffix.is_empty() {
        return true;
    }
    let Some(port) = suffix.strip_prefix(':') else {
        return false;
    };
    !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && port.parse::<u16>().is_ok()
}

fn validate_loopback_host_header(headers: &HeaderMap) -> std::result::Result<&str, ApiError> {
    let mut values = headers.get_all(HOST).iter();
    let value = values
        .next()
        .filter(|_| values.next().is_none())
        .ok_or_else(ApiError::invalid_workbench_host)?;
    let host = value
        .to_str()
        .map_err(|_| ApiError::invalid_workbench_host())?;
    let authority = host
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| ApiError::invalid_workbench_host())?;
    if host.contains('@')
        || !authority_port_is_valid(&authority)
        || !host_is_loopback(authority.host())
    {
        return Err(ApiError::invalid_workbench_host());
    }
    Ok(host)
}

fn bounded_identifier_path(
    path: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
) -> std::result::Result<String, ApiError> {
    let AxumPath(value) = path.map_err(|_| ApiError::invalid_request())?;
    if value.is_empty()
        || value.len() > 256
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(ApiError::invalid_request());
    }
    Ok(value)
}

fn identifier_transport_is_safe(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn sha256_transport_is_safe(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn idempotency_key_transport_is_safe(value: &str) -> bool {
    value.len() >= MINIMUM_IDEMPOTENCY_KEY_BYTES
        && value.len() <= MAXIMUM_IDEMPOTENCY_KEY_BYTES
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_result_attribution(actor: &str, reason: &str) -> bool {
    !actor.is_empty()
        && actor.trim() == actor
        && actor.len() <= 256
        && !actor.chars().any(char::is_control)
        && !reason.trim().is_empty()
        && reason.len() <= 4_096
        && !reason.chars().any(char::is_control)
}

fn valid_result_action_fields(
    candidate_hash: &str,
    actor: &str,
    reason: &str,
    warning_acknowledgments: &[String],
    acknowledge_local_only_boundary: bool,
) -> bool {
    acknowledge_local_only_boundary
        && sha256_transport_is_safe(candidate_hash)
        && valid_result_attribution(actor, reason)
        && warning_acknowledgments.len() <= 200
        && warning_acknowledgments
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        && warning_acknowledgments.iter().all(|code| {
            !code.is_empty()
                && code.len() <= 256
                && code.as_bytes()[0].is_ascii_uppercase()
                && code.bytes().all(|byte| {
                    byte.is_ascii_uppercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b':' | b'-')
                })
        })
}

fn validate_reconciliation_row_transport(
    filters: &ReconciliationRowFilters,
) -> std::result::Result<(), ApiError> {
    const CATEGORIES: [&str; 8] = [
        "INCLUDED_VALID",
        "INCLUDED_CORRECTED",
        "INCLUDED_WAIVED",
        "INCLUDED_ACCEPTED_AS_IS",
        "EXCLUDED",
        "BLOCKED_UNRESOLVED",
        "BLOCKED_INCOMPLETE",
        "BLOCKED_CONFLICT",
    ];
    if filters
        .category
        .as_deref()
        .is_some_and(|category| !CATEGORIES.contains(&category))
    {
        return Err(ApiError::invalid_request());
    }
    if filters.source_file.as_deref().is_some_and(|source_file| {
        source_file.is_empty()
            || source_file.len() > 1_024
            || !source_file_transport_is_safe(source_file)
    }) {
        return Err(ApiError::invalid_request());
    }
    if filters.source_ordinal == Some(0) {
        return Err(ApiError::invalid_request());
    }
    if [
        filters.entity_id.as_deref(),
        filters.issue_id.as_deref(),
        filters.decision_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|identifier| !identifier_transport_is_safe(identifier))
    {
        return Err(ApiError::invalid_request());
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn derive_decision_request(
    issue_id: &str,
    idempotency_key: String,
    action: GovernedAction,
    governance: &SelectedGovernance,
    detail: &DecisionAwareIssueDetail,
) -> std::result::Result<IssueDecisionRequest, ApiError> {
    let issue = &detail.data;
    if issue.issue_id != issue_id || issue.validation_run_id != governance.validation_run_id {
        return Err(ApiError::invalid_command());
    }

    let (common, expected_schema, disposition, expected_effective_value_hash, overlay, row_ack) =
        match action {
            GovernedAction::Correct(command) => {
                let CorrectIssueCommand {
                    schema_version,
                    validation_run_id,
                    issue_hash,
                    practitioner_identifier,
                    reason,
                    evidence_reference,
                    expected_active_decision_id,
                    expected_active_generation,
                    acknowledge_not_migration_result_approval,
                    correction,
                } = command;
                if correction.source_file != issue.accepted_row.source_file
                    || correction.source_ordinal != issue.accepted_row.source_ordinal
                    || correction.entity_id != issue.entity_id
                    || correction.field_id != issue.field_id
                {
                    return Err(ApiError::invalid_command());
                }
                let replacement_text = match (correction.operation, correction.value.0) {
                    (CorrectionOperation::ReplaceEffectiveText, Some(value))
                        if !value.is_empty() && value.chars().count() <= 65_536 =>
                    {
                        Some(value)
                    }
                    (CorrectionOperation::SetEffectiveEmpty, Some(value)) if value.is_empty() => {
                        None
                    }
                    (CorrectionOperation::SetEffectiveNull, None) => None,
                    _ => return Err(ApiError::invalid_command()),
                };
                (
                    CommonDecisionCommand {
                        schema_version,
                        validation_run_id,
                        issue_hash,
                        practitioner_identifier,
                        reason,
                        evidence_reference: evidence_reference.0,
                        expected_active_decision_id: expected_active_decision_id.0,
                        expected_active_generation,
                        acknowledge_not_migration_result_approval,
                    },
                    "migration-stream/write/correct-issue/v1",
                    IssueDisposition::Corrected,
                    Some(correction.expected_effective_value_hash),
                    Some(CorrectionOverlayRequest {
                        operation: correction.operation,
                        replacement_text,
                    }),
                    false,
                )
            }
            GovernedAction::Waive(command) => {
                let WaiveIssueCommand {
                    schema_version,
                    validation_run_id,
                    issue_hash,
                    practitioner_identifier,
                    reason,
                    evidence_reference,
                    expected_active_decision_id,
                    expected_active_generation,
                    acknowledge_not_migration_result_approval,
                    acknowledge_waiver_limitations,
                } = command;
                if !acknowledge_waiver_limitations {
                    return Err(ApiError::invalid_command());
                }
                (
                    CommonDecisionCommand {
                        schema_version,
                        validation_run_id,
                        issue_hash,
                        practitioner_identifier,
                        reason,
                        evidence_reference: evidence_reference.0,
                        expected_active_decision_id: expected_active_decision_id.0,
                        expected_active_generation,
                        acknowledge_not_migration_result_approval,
                    },
                    "migration-stream/write/waive-issue/v1",
                    IssueDisposition::Waived,
                    None,
                    None,
                    false,
                )
            }
            GovernedAction::AcceptAsIs(command) => {
                let AcceptIssueAsIsCommand {
                    schema_version,
                    validation_run_id,
                    issue_hash,
                    practitioner_identifier,
                    reason,
                    evidence_reference,
                    expected_active_decision_id,
                    expected_active_generation,
                    acknowledge_not_migration_result_approval,
                    expected_effective_value_hash,
                    acknowledge_validation_truth_unchanged,
                } = command;
                if !acknowledge_validation_truth_unchanged {
                    return Err(ApiError::invalid_command());
                }
                (
                    CommonDecisionCommand {
                        schema_version,
                        validation_run_id,
                        issue_hash,
                        practitioner_identifier,
                        reason,
                        evidence_reference: evidence_reference.0,
                        expected_active_decision_id: expected_active_decision_id.0,
                        expected_active_generation,
                        acknowledge_not_migration_result_approval,
                    },
                    "migration-stream/write/accept-issue-as-is/v1",
                    IssueDisposition::AcceptedAsIs,
                    Some(expected_effective_value_hash),
                    None,
                    false,
                )
            }
            GovernedAction::ExcludeRow(command) => {
                let ExcludeIssueRowCommand {
                    schema_version,
                    validation_run_id,
                    issue_hash,
                    practitioner_identifier,
                    reason,
                    evidence_reference,
                    expected_active_decision_id,
                    expected_active_generation,
                    acknowledge_not_migration_result_approval,
                    source_file,
                    source_ordinal,
                    acknowledge_row_scope,
                } = command;
                if !acknowledge_row_scope
                    || source_file != issue.accepted_row.source_file
                    || source_ordinal != issue.accepted_row.source_ordinal
                {
                    return Err(ApiError::invalid_command());
                }
                (
                    CommonDecisionCommand {
                        schema_version,
                        validation_run_id,
                        issue_hash,
                        practitioner_identifier,
                        reason,
                        evidence_reference: evidence_reference.0,
                        expected_active_decision_id: expected_active_decision_id.0,
                        expected_active_generation,
                        acknowledge_not_migration_result_approval,
                    },
                    "migration-stream/write/exclude-issue-row/v1",
                    IssueDisposition::Excluded,
                    None,
                    None,
                    true,
                )
            }
        };

    if common.schema_version != expected_schema
        || common.validation_run_id != issue.validation_run_id
        || common.issue_hash != issue.issue_hash
        || !common.acknowledge_not_migration_result_approval
    {
        return Err(ApiError::invalid_command());
    }

    Ok(IssueDecisionRequest {
        schema_version: DECISION_SCHEMA_VERSION,
        idempotency_key,
        disposition,
        scope: DecisionScope {
            job_id: issue.job_id.clone(),
            validation_run_id: issue.validation_run_id.clone(),
            issue_id: issue.issue_id.clone(),
            issue_hash: issue.issue_hash.clone(),
            accepted_source_file: issue.accepted_row.source_file.clone(),
            source_ordinal: issue.accepted_row.source_ordinal,
            line_start: issue.accepted_row.line_start,
            line_end: issue.accepted_row.line_end,
            canonical_entity_id: issue.entity_id.clone(),
            canonical_field_id: issue.field_id.clone(),
            profile_id: governance.profile_id.clone(),
            profile_version: governance.profile_version.clone(),
            profile_hash: governance.profile_hash.clone(),
            mapping_id: governance.mapping_id.clone(),
            mapping_version: governance.mapping_version.clone(),
            mapping_hash: governance.mapping_hash.clone(),
        },
        concurrency: DecisionConcurrency {
            expected_active_decision_id: common.expected_active_decision_id,
            expected_active_decision_generation: common.expected_active_generation,
            expected_effective_value_hash,
        },
        practitioner_id: common.practitioner_identifier,
        reason: common.reason,
        evidence_reference: common.evidence_reference,
        authority_separation_acknowledged: true,
        row_scope_exclusion_acknowledged: row_ack,
        correction_overlay: overlay,
    })
}

fn validate_page_transport(page: &PageRequest) -> std::result::Result<(), ApiError> {
    if page.page_size == 0 || page.page_size > 200 {
        return Err(ApiError::invalid_request());
    }
    if page
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.len() > MAXIMUM_QUERY_BYTES)
    {
        return Err(ApiError::invalid_request());
    }
    Ok(())
}

fn validate_issue_transport(filters: &IssueFilters) -> std::result::Result<(), ApiError> {
    for value in [
        filters.rule_id.as_ref(),
        filters.entity_id.as_ref(),
        filters.field_id.as_ref(),
        filters.source_file.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if value.is_empty() || value.len() > MAXIMUM_FILTER_BYTES {
            return Err(ApiError::invalid_request());
        }
    }
    if filters.source_ordinal == Some(0) {
        return Err(ApiError::invalid_request());
    }
    if filters
        .source_file
        .as_deref()
        .is_some_and(|source_file| !source_file_transport_is_safe(source_file))
    {
        return Err(ApiError::invalid_request());
    }
    Ok(())
}

fn source_file_transport_is_safe(source_file: &str) -> bool {
    let path = Path::new(source_file);
    !path.is_absolute()
        && !source_file.chars().any(char::is_control)
        && !path.components().any(|component| {
            if !matches!(component, std::path::Component::Normal(_)) {
                return true;
            }
            let std::path::Component::Normal(value) = component else {
                return true;
            };
            value
                .to_string_lossy()
                .rsplit_once('.')
                .is_some_and(|(_, extension)| {
                    extension.eq_ignore_ascii_case("part")
                        || extension.eq_ignore_ascii_case("tombstone")
                })
        })
        && !crate::durability::path_uses_tombstone_namespace(path)
}

async fn method_not_allowed() -> impl IntoResponse {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "METHOD_NOT_ALLOWED",
        "The requested method is not available for this local API route.",
        false,
    )
}

async fn route_not_found() -> impl IntoResponse {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "API_ROUTE_NOT_FOUND",
        "The requested local API route does not exist.",
        false,
    )
}

async fn enforce_workspace_identity(
    State(state): State<WorkbenchState>,
    request: Request<Body>,
    next: Next,
) -> axum::response::Response {
    if let Some(error) = retained_identity_error(state.identity.clone()).await {
        return error.into_response();
    }
    let response = next.run(request).await;
    if let Some(error) = retained_identity_error(state.identity).await {
        return error.into_response();
    }
    response
}

enum RetainedIdentityVerification {
    Verified,
    PhysicalIdentityChanged,
    AuthorityUnavailable(KernelError),
}

async fn retained_identity_error(identity: WorkspaceIdentity) -> Option<ApiError> {
    let verification = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::task::spawn_blocking(move || {
            if identity.verify_retained_paths().is_err() {
                return RetainedIdentityVerification::PhysicalIdentityChanged;
            }
            match identity.verify_workspace_authority() {
                Ok(()) => RetainedIdentityVerification::Verified,
                Err(error) => RetainedIdentityVerification::AuthorityUnavailable(error),
            }
        }),
    )
    .await;
    match verification {
        Ok(Ok(RetainedIdentityVerification::Verified)) => None,
        Ok(Ok(RetainedIdentityVerification::AuthorityUnavailable(error))) => {
            Some(ApiError::from_kernel(error))
        }
        Ok(Ok(RetainedIdentityVerification::PhysicalIdentityChanged) | Err(_)) | Err(_) => {
            Some(ApiError::new(
                StatusCode::CONFLICT,
                "WORKSPACE_IDENTITY_CHANGED",
                "The startup-selected workspace identity is no longer available.",
                false,
            ))
        }
    }
}

async fn enforce_transport_bounds(request: Request<Body>, next: Next) -> axum::response::Response {
    let uri_bytes = request.uri().to_string().len();
    let query_bytes = request.uri().query().map_or(0, str::len);
    let header_bytes = header_size(request.headers());
    if uri_bytes > MAXIMUM_URI_BYTES
        || query_bytes > MAXIMUM_QUERY_BYTES
        || request.headers().len() > MAXIMUM_HEADER_COUNT
        || header_bytes.is_none_or(|bytes| bytes > MAXIMUM_HEADER_BYTES)
    {
        return ApiError::invalid_request().into_response();
    }
    if let Err(error) = validate_loopback_host_header(request.headers()) {
        return error.into_response();
    }
    if matches!(request.method(), &Method::GET | &Method::HEAD)
        && request_has_body(request.headers())
    {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "REQUEST_BODY_NOT_ALLOWED",
            "Read-only API requests cannot contain a request body.",
            false,
        )
        .into_response();
    }
    if request.method() == Method::POST {
        if request.uri().query().is_some() {
            return ApiError::invalid_request().into_response();
        }
        if request
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|length| length > MAXIMUM_COMMAND_BODY_BYTES)
        {
            return if request.uri().path().starts_with("/api/v1/result-") {
                ApiError::result_body_too_large().into_response()
            } else {
                ApiError::command_body_too_large().into_response()
            };
        }
    }
    next.run(request).await
}

pub(crate) async fn add_security_headers(
    request: Request<Body>,
    next: Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    insert_static_header(headers, "x-content-type-options", "nosniff");
    insert_static_header(headers, "x-frame-options", "DENY");
    insert_static_header(
        headers,
        "content-security-policy",
        "default-src 'self'; object-src 'none'; frame-ancestors 'none'; base-uri 'none'",
    );
    insert_static_header(headers, "cross-origin-resource-policy", "same-origin");
    insert_static_header(headers, "cross-origin-opener-policy", "same-origin");
    insert_static_header(
        headers,
        "permissions-policy",
        "camera=(), geolocation=(), microphone=()",
    );
    response
}

fn insert_static_header(headers: &mut HeaderMap, name: &'static str, value: &'static str) {
    headers.insert(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    );
}

fn header_size(headers: &HeaderMap) -> Option<usize> {
    headers.iter().try_fold(0_usize, |total, (name, value)| {
        total
            .checked_add(name.as_str().len())?
            .checked_add(value.as_bytes().len())
    })
}

fn request_has_body(headers: &HeaderMap) -> bool {
    if headers.contains_key("transfer-encoding") {
        return true;
    }
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value != "0")
}

fn supported_contracts(
    governed_decisions: bool,
    migration_result_approval: bool,
    readiness_publication: bool,
) -> Vec<&'static str> {
    let mut contracts = vec![
        "migration-stream/read/job-summary/v1",
        "migration-stream/read/file-manifest/v1",
        "migration-stream/read/profiling-summary/v1",
        "migration-stream/read/profile-view/v1",
        "migration-stream/read/mapping-coverage/v1",
        "migration-stream/read/validation-summary/v1",
        "migration-stream/read/issue-list/v1",
        "migration-stream/read/row-detail/v1",
        "migration-stream/read/reconciliation-summary/v1",
        "migration-stream/read/operational-cleanup/v1",
    ];
    if governed_decisions {
        contracts.extend([
            "migration-stream/workbench/context/v1",
            "migration-stream/workbench/context/v2",
            "migration-stream/read/decision-queue/v1",
            "migration-stream/read/decision-counts/v1",
            "migration-stream/read/decision-aware-issue-detail/v1",
            "migration-stream/read/decision-history/v1",
            "migration-stream/read/correction-overlay-detail/v1",
            "migration-stream/read/decision-result/v1",
            "migration-stream/write/decision-command-result/v1",
            "migration-stream/http-error/v1",
        ]);
    }
    if migration_result_approval {
        contracts.extend([
            RESULT_APPROVAL_CONTEXT_SCHEMA,
            "migration-stream/read/result-reconciliation-snapshot-list/v1",
            "migration-stream/read/result-reconciliation-snapshot-summary/v1",
            "migration-stream/read/result-reconciliation-row-list/v1",
            "migration-stream/read/result-reconciliation-row-detail/v1",
            "migration-stream/read/result-candidate-list/v1",
            "migration-stream/read/result-approval-eligibility/v1",
            "migration-stream/read/result-action-history/v1",
            RECONCILIATION_COMMAND_RESULT_SCHEMA,
            CANDIDATE_COMMAND_RESULT_SCHEMA,
            ACTION_COMMAND_RESULT_SCHEMA,
        ]);
    }
    if readiness_publication {
        contracts.extend([
            PUBLICATION_CONTEXT_SCHEMA,
            PUBLICATION_ELIGIBILITY_SCHEMA,
            PUBLICATION_LIST_SCHEMA,
            crate::publication::PUBLICATION_DETAIL_SCHEMA,
            PUBLICATION_COMMAND_SCHEMA,
            PUBLICATION_CONTRACT_VERSION,
        ]);
    }
    contracts
}

fn next_request_id() -> String {
    let sequence = REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("request-{sequence:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(HOST, HeaderValue::from_static("127.0.0.1:4173"));
        headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.1:4173"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "x-migration-stream-intent",
            HeaderValue::from_static(RESULT_APPROVAL_INTENT),
        );
        headers.insert(
            "idempotency-key",
            HeaderValue::from_static("result-command-0001"),
        );
        headers
    }

    #[test]
    fn network_authority_is_bounded() {
        assert!(validate_loopback_address(DEFAULT_LISTEN_ADDRESS).is_ok());
        assert!(validate_loopback_address(SocketAddr::from(([0, 0, 0, 0], 4173))).is_err());
        assert!(validate_loopback_address(SocketAddr::from(([192, 0, 2, 1], 4173))).is_err());
    }

    #[test]
    fn deadline_and_response_bounds_map_to_stable_transport_failures() {
        let deadline = ApiError::from_kernel(KernelError::ReadQueryGuard);
        assert_eq!(deadline.status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(deadline.code, "QUERY_DEADLINE_EXCEEDED");
        assert!(deadline.retryable);

        let oversized = bounded_json(vec!["bounded"; 16], 8);
        assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            oversized.headers().get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
    }

    #[test]
    fn result_capability_is_separate_and_keeps_negative_authority() {
        let config =
            WorkbenchConfig::new("workspace", "job-1", None).with_migration_result_approval();
        assert!(config.governed_decisions);
        assert!(config.migration_result_approval);
        assert!(!config.readiness_publication);

        let publication =
            WorkbenchConfig::new("workspace", "job-1", None).with_readiness_publication();
        assert!(publication.governed_decisions);
        assert!(publication.migration_result_approval);
        assert!(publication.readiness_publication);

        let read_only = supported_contracts(false, false, false);
        let decisions = supported_contracts(true, false, false);
        let result = supported_contracts(true, true, false);
        let publication = supported_contracts(true, true, true);
        assert!(!read_only.contains(&RESULT_APPROVAL_CONTEXT_SCHEMA));
        assert!(!decisions.contains(&RESULT_APPROVAL_CONTEXT_SCHEMA));
        assert!(result.contains(&RESULT_APPROVAL_CONTEXT_SCHEMA));
        assert!(result.contains(&ACTION_COMMAND_RESULT_SCHEMA));
        assert!(!result.contains(&PUBLICATION_CONTEXT_SCHEMA));
        assert!(!result.contains(&PUBLICATION_ELIGIBILITY_SCHEMA));
        assert!(publication.contains(&PUBLICATION_CONTEXT_SCHEMA));
        assert!(publication.contains(&PUBLICATION_ELIGIBILITY_SCHEMA));
        assert!(publication.contains(&PUBLICATION_LIST_SCHEMA));
        assert!(publication.contains(&crate::publication::PUBLICATION_DETAIL_SCHEMA));
        assert!(publication.contains(&PUBLICATION_COMMAND_SCHEMA));
        assert!(publication.contains(&PUBLICATION_CONTRACT_VERSION));
    }

    #[test]
    fn result_transport_requires_exact_same_origin_intent_and_retry_identity() {
        let headers = result_headers();
        assert_eq!(
            validate_result_headers(&headers).expect("valid bounded WP-006 transport"),
            "result-command-0001"
        );

        let mut wrong_intent = headers.clone();
        wrong_intent.insert(
            "x-migration-stream-intent",
            HeaderValue::from_static(GOVERNED_INTENT),
        );
        assert_eq!(
            validate_result_headers(&wrong_intent)
                .expect_err("governed intent must not authorize WP-006")
                .code,
            "INVALID_RESULT_TRANSPORT"
        );

        let mut forwarded = headers;
        forwarded.insert("x-forwarded-host", HeaderValue::from_static("localhost"));
        assert!(validate_result_headers(&forwarded).is_err());
    }

    #[test]
    fn result_action_transport_enforces_sorted_closed_warning_codes() {
        assert!(valid_result_action_fields(
            &"a".repeat(64),
            "Practitioner",
            "Reviewed locally",
            &["FIRST_WARNING".to_owned(), "SECOND_WARNING".to_owned()],
            true,
        ));
        assert!(!valid_result_action_fields(
            &"a".repeat(64),
            "Practitioner",
            "Reviewed locally",
            &["SECOND_WARNING".to_owned(), "FIRST_WARNING".to_owned()],
            true,
        ));
        assert!(!valid_result_action_fields(
            &"a".repeat(64),
            "Practitioner",
            "Reviewed locally",
            &[],
            false,
        ));
    }

    #[test]
    fn action_head_binding_requires_exact_latest_generation() {
        let item = ResultActionSummary {
            action_id: "action-1".to_owned(),
            action_kind: "APPROVED".to_owned(),
            action_hash: "a".repeat(64),
            candidate_id: "candidate-1".to_owned(),
            candidate_hash: "b".repeat(64),
            actor: "Practitioner".to_owned(),
            reason: "Reviewed locally".to_owned(),
            generation: 3,
            predecessor_action_id: None,
            occurred_at: "2026-07-22T00:00:00Z".to_owned(),
        };
        let head = action_head_from_history(std::slice::from_ref(&item), 3)
            .expect("matching immutable head");
        assert_eq!(head.action_id.as_deref(), Some("action-1"));
        assert_eq!(head.action_hash.as_deref(), Some(item.action_hash.as_str()));
        assert!(action_head_from_history(std::slice::from_ref(&item), 2).is_err());
        assert!(action_head_from_history(&[], 1).is_err());
        assert!(action_head_from_history(&[], 0).is_ok());
    }

    #[test]
    fn result_errors_map_without_weakening_integrity_or_conflict() {
        let invalid = ApiError::from_kernel(KernelError::ResultApprovalInvalid("x".to_owned()));
        assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
        assert_eq!(invalid.code, "INVALID_RESULT_COMMAND");

        let conflict = ApiError::from_kernel(KernelError::ResultApprovalConflict("x".to_owned()));
        assert_eq!(conflict.status, StatusCode::CONFLICT);
        assert_eq!(conflict.code, "RESULT_APPROVAL_CONFLICT");

        let integrity = ApiError::from_kernel(KernelError::ResultApprovalIntegrity("x".to_owned()));
        assert_eq!(integrity.status, StatusCode::CONFLICT);
        assert_eq!(integrity.code, "RESULT_APPROVAL_INTEGRITY_FAILED");
    }
}

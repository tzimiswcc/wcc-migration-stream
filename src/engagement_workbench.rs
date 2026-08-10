//! Loopback-only browser transport for one server-controlled local engagement.
//!
//! Setup requests select no server filesystem paths and cannot replace the
//! startup-fixed policies, schemas, or reviewed object-pack catalogue. Once
//! validation completes, ordinary read and governed-Issues requests are handed
//! to the existing practitioner workbench for the exact active job.

use std::{
    collections::HashSet,
    fs,
    net::IpAddr,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Path as AxumPath, Query, State, rejection::QueryRejection},
    http::{
        HeaderMap, HeaderValue, Method, Request, StatusCode, Uri,
        header::{CONTENT_LENGTH, CONTENT_TYPE, HOST, ORIGIN},
    },
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::{get, post, put},
};
use http_body_util::BodyExt as _;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;
use tower::ServiceExt as _;

use crate::{
    KernelError, Result,
    canonical::{canonical_hash, canonical_json_bytes, parse_unique_json},
    destination::{
        APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA, ApproveDestinationProfileCommand,
        CARRY_FORWARD_MAPPING_COMMAND_SCHEMA, CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA,
        CarryForwardMappingCommand, CreateManualCatalogueCommand, DestinationFieldState,
        FREEZE_DRAFT_COMMAND_SCHEMA, FreezeDestinationDraftCommand, REBASE_DRAFT_COMMAND_SCHEMA,
        RESERVE_EDMX_COMMAND_SCHEMA, RebaseDestinationDraftCommand, ReserveEdmxCommand,
        SAVE_DRAFT_COMMAND_SCHEMA, START_DRAFT_COMMAND_SCHEMA, SaveDestinationDraftCommand,
        StartDestinationDraftCommand,
    },
    engagement::{
        ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA, ACCEPT_SOURCE_COMMAND_SCHEMA,
        APPROVE_MAPPING_VERSION_COMMAND_SCHEMA, APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA,
        AcceptPreflightCommand, AcceptRevisedPreflightCommand, ActiveEngagementIds,
        ApproveMappingVersionCommand, ApproveProfileCommand, CREATE_ENGAGEMENT_COMMAND_SCHEMA,
        CREATE_REMEDIATION_PACK_COMMAND_SCHEMA, CompletePreflightCommand, ControlledObjectPack,
        CreateOrReopenCommand, CreateRemediationPackCommand, EngagementPhase, EngagementService,
        EngagementServiceConfig, EngagementState, LAUNCH_REVISED_VALIDATION_COMMAND_SCHEMA,
        LAUNCH_VALIDATION_COMMAND_SCHEMA, LaunchRevisedValidationCommand, LaunchValidationCommand,
        MappingMode, ObjectPackMappingSuggestion, PREPARE_MAPPING_COMMAND_SCHEMA,
        PreflightTerminalCommand, RESERVE_PREFLIGHT_COMMAND_SCHEMA,
        RESERVE_REVISED_PREFLIGHT_COMMAND_SCHEMA, ReservePreflightCommand,
        ReserveRevisedPreflightCommand, SAVE_MAPPING_DRAFT_COMMAND_SCHEMA,
        SELECT_OBJECT_PACK_COMMAND_SCHEMA, START_REVISED_TOUR_COMMAND_SCHEMA,
        SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA, SaveMappingDraftCommand, SelectObjectPackCommand,
        StartRevisedTourCommand, SubmitMappingDraftCommand,
    },
    mapping::{MappingKind, ReferenceTable},
    object_workspace::{
        ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA, EstablishObjectWorkspaceCommand,
        INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA,
    },
    policy::IntakePolicy,
    profile::validate_profile,
    profiling::ProfilingPolicy,
    publication_read_model::PublicationReadService,
    read_model::{PageRequest, SelectedGovernance},
    reconciliation_read_model::ReconciliationReadService,
    validation::ValidationPolicy,
    workbench::{
        StaticAssets, WorkbenchConfig, add_security_headers, build_router, static_fallback,
    },
};

const ERROR_SCHEMA: &str = "migration-stream/http-error/v1";
const MAXIMUM_COMMAND_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAXIMUM_MANIFEST_BYTES: usize = 1024 * 1024;
const MAXIMUM_OBJECT_PACKS: usize = 16;
const MAXIMUM_URI_BYTES: usize = 12 * 1024;
const MAXIMUM_HEADER_COUNT: usize = 64;
const MAXIMUM_HEADER_BYTES: usize = 32 * 1024;
const MINIMUM_IDEMPOTENCY_KEY_BYTES: usize = 16;
const MAXIMUM_IDEMPOTENCY_KEY_BYTES: usize = 128;

const CREATE_INTENT: &str = "engagement-create-v1";
const INITIALIZE_OBJECT_WORKSPACE_INTENT: &str = "object-workspace-initialize-v1";
const ADOPT_OBJECT_WORKSPACE_INTENT: &str = "object-workspace-adopt-v1";
const RESERVE_INTENT: &str = "engagement-source-preflight-reserve-v1";
const CONTENT_INTENT: &str = "engagement-source-preflight-content-v1";
const ACCEPT_INTENT: &str = "engagement-source-accept-v1";
const SELECT_PACK_INTENT: &str = "engagement-object-pack-select-v1";
const APPROVE_PROFILE_INTENT: &str = "engagement-profile-approval-v1";
const PREPARE_MAPPING_INTENT: &str = "engagement-mapping-prepare-v1";
const SAVE_MAPPING_DRAFT_INTENT: &str = "engagement-mapping-draft-save-v1";
const SUBMIT_MAPPING_DRAFT_INTENT: &str = "engagement-mapping-draft-submit-v1";
const APPROVE_MAPPING_VERSION_INTENT: &str = "engagement-mapping-version-approve-v1";
const LAUNCH_VALIDATION_INTENT: &str = "engagement-validation-launch-v1";
const START_REVISED_TOUR_INTENT: &str = "engagement-revised-tour-start-v1";
const RESERVE_REVISED_SOURCE_INTENT: &str = "engagement-revised-source-reserve-v1";
const ACCEPT_REVISED_SOURCE_INTENT: &str = "engagement-revised-source-accept-v1";
const LAUNCH_REVISED_VALIDATION_INTENT: &str = "engagement-revised-validation-launch-v1";
const CREATE_REMEDIATION_PACK_INTENT: &str = "engagement-remediation-pack-create-v1";
const DESTINATION_EDMX_RESERVE_INTENT: &str = "engagement-destination-edmx-reserve-v1";
const DESTINATION_EDMX_CONTENT_INTENT: &str = "engagement-destination-edmx-content-v1";
const DESTINATION_MANUAL_INTENT: &str = "engagement-destination-manual-catalogue-v1";
const DESTINATION_DRAFT_START_INTENT: &str = "engagement-destination-draft-start-v1";
const DESTINATION_DRAFT_SAVE_INTENT: &str = "engagement-destination-draft-save-v1";
const DESTINATION_DRAFT_REBASE_INTENT: &str = "engagement-destination-draft-rebase-v1";
const DESTINATION_DRAFT_FREEZE_INTENT: &str = "engagement-destination-draft-freeze-v1";
const DESTINATION_PROFILE_APPROVE_INTENT: &str = "engagement-destination-profile-approve-v1";
const DESTINATION_MAPPING_CARRY_FORWARD_INTENT: &str =
    "engagement-destination-mapping-carry-forward-v1";
const MAXIMUM_EDMX_BYTES: u64 = 8 * 1024 * 1024;
const ASSISTED_RUNTIME_IDENTITY_SCHEMA: &str =
    "migration-stream/assisted-local-runtime/application-identity/v2";
const ASSISTED_RUNTIME_IDENTITY_HEADER: &str = "x-migration-stream-runtime-token";

static REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Startup-fixed dependencies for the assisted engagement surface.
#[derive(Clone, Debug)]
pub struct EngagementWorkbenchConfig {
    pub workspace: PathBuf,
    pub intake_policy: PathBuf,
    pub profiling_policy: PathBuf,
    pub validation_policy: PathBuf,
    pub object_packs: PathBuf,
    pub profile_schema: PathBuf,
    pub mapping_schema: PathBuf,
}

/// Session-local identity route enabled only by the verified assisted runtime.
#[derive(Clone, Debug)]
pub struct AssistedRuntimeIdentityConfig {
    pub session_token: String,
    pub listen_address: String,
}

#[derive(Clone)]
struct EngagementWorkbenchState {
    service: Arc<EngagementService>,
    assets: StaticAssets,
    maximum_source_bytes: u64,
    profile_schema: PathBuf,
    assisted_runtime_identity: Option<AssistedRuntimeIdentityState>,
    downstream_router: Arc<tokio::sync::Mutex<Option<CachedDownstreamRouter>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DownstreamRouterAuthority {
    engagement_id: String,
    generation: u64,
    phase: EngagementPhase,
    active: ActiveEngagementIds,
    profile_stage: String,
    mapping_stage: String,
    validation_stage: String,
    capabilities: DownstreamCapabilityState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DownstreamCapabilityState {
    ReadOnly,
    MultiEntityLocked,
    GovernedLocalReadiness,
}

#[derive(Clone)]
struct CachedDownstreamRouter {
    authority: DownstreamRouterAuthority,
    router: Router,
}

#[derive(Clone)]
struct AssistedRuntimeIdentityState {
    session_token_sha256: String,
    listen_address: String,
    process_id: u32,
    workspace_path: PathBuf,
    filesystem_identity_sha256: String,
}

/// Runtime-local filesystem authority. The canonical path binds the selected
/// startup scope; the digest is the sole native filesystem-object identity.
#[derive(Clone, Debug)]
struct RuntimeFilesystemIdentity {
    canonical_path: PathBuf,
    filesystem_identity_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DestinationCatalogueEntitiesQuery {
    catalogue_authority_id: String,
    query: Option<String>,
    page_size: Option<usize>,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DestinationCataloguePropertiesQuery {
    catalogue_authority_id: String,
    technical_entity: String,
    query: Option<String>,
    state: Option<DestinationFieldState>,
    page_size: Option<usize>,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ObjectPackManifest {
    schema_version: u32,
    pack_id: String,
    version: String,
    title: String,
    scope: String,
    root_entity_id: String,
    #[serde(default)]
    mapping_mode: MappingMode,
    profile_file: String,
    profile_sha256: String,
    mapping_suggestions: Vec<ManifestMappingSuggestion>,
    reference_tables: Vec<ReferenceTable>,
    limitations: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestMappingSuggestion {
    suggestion_id: String,
    source_file: Option<String>,
    source_column: Option<String>,
    source_column_ordinal: Option<usize>,
    target_entity_id: Option<String>,
    target_field_id: String,
    kind: MappingKind,
    constant: Option<String>,
    lookup_table_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    schema_version: &'static str,
    ok: bool,
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: String,
    message: &'static str,
    request_id: String,
    retryable: bool,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: String,
    message: &'static str,
    retryable: bool,
}

impl ApiError {
    fn new(
        status: StatusCode,
        code: impl Into<String>,
        message: &'static str,
        retryable: bool,
    ) -> Self {
        Self {
            status,
            code: code.into(),
            message,
            retryable,
        }
    }

    fn invalid_request() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INVALID_ENGAGEMENT_REQUEST",
            "The local engagement request is invalid or outside its supported bounds.",
            false,
        )
    }

    fn invalid_transport() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "INVALID_ENGAGEMENT_TRANSPORT",
            "The command did not satisfy the same-origin local transport contract.",
            false,
        )
    }

    fn from_kernel(error: &KernelError) -> Self {
        let code = error.code();
        match error {
            KernelError::NotFound(_) => Self::new(
                StatusCode::NOT_FOUND,
                code,
                "The requested local engagement evidence was not found.",
                false,
            ),
            KernelError::WorkspaceLocked(_) => Self::new(
                StatusCode::CONFLICT,
                code,
                "The local engagement workspace is currently in use; retry the unchanged action.",
                true,
            ),
            KernelError::SourceSize { .. } => Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                code,
                "The selected ZIP exceeds the configured local source-size limit.",
                false,
            ),
            KernelError::ArchiveMissingCsv => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                code,
                "The selected ZIP does not contain a supported CSV source.",
                false,
            ),
            KernelError::ArchiveEncrypted(_)
            | KernelError::ArchiveAbsolutePath(_)
            | KernelError::ArchivePathTraversal(_)
            | KernelError::ArchiveUnsafePath(_)
            | KernelError::ArchiveLink(_)
            | KernelError::ArchiveDuplicatePath(_)
            | KernelError::ArchiveEntryLimit { .. }
            | KernelError::ArchiveExpandedSize { .. }
            | KernelError::ArchiveCompressionRatio { .. }
            | KernelError::ArchiveIntegrity(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                code,
                "The selected ZIP failed the bounded archive safety preflight.",
                false,
            ),
            KernelError::CsvSyntax { .. }
            | KernelError::CsvFieldLimit { .. }
            | KernelError::CsvRecordLimit { .. }
            | KernelError::CsvColumnLimit { .. }
            | KernelError::CsvRecordCountLimit { .. }
            | KernelError::CsvEmptyHeader(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                code,
                "A CSV in the selected ZIP does not satisfy the supported bounded source shape.",
                false,
            ),
            KernelError::ValidationBound(_)
            | KernelError::DecisionBound(_)
            | KernelError::ResultApprovalBound(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                code,
                "The operation stopped at an explicit local safety bound.",
                false,
            ),
            KernelError::Conflict(_)
            | KernelError::MappingConflict(_)
            | KernelError::ProfileApproval(_)
            | KernelError::ProfileNotApproved(_)
            | KernelError::ProfileRegistry(_)
            | KernelError::IntegrityFailed(_)
            | KernelError::GeneratedDraftAuthority => Self::new(
                StatusCode::CONFLICT,
                code,
                "The action conflicts with the current immutable evidence or authority state.",
                false,
            ),
            KernelError::PolicyInvalid(_)
            | KernelError::ProfileInvalid(_)
            | KernelError::ProfileSemantic(_)
            | KernelError::MappingInvalid(_)
            | KernelError::ValidationInvalid(_)
            | KernelError::ReadModel(_)
            | KernelError::DecisionInvalid(_)
            | KernelError::ResultApprovalInvalid(_) => Self::new(
                StatusCode::BAD_REQUEST,
                code,
                "The command is invalid or outside the controlled engagement contract.",
                false,
            ),
            _ => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                code,
                "The bounded local engagement service is unavailable.",
                false,
            ),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                schema_version: ERROR_SCHEMA,
                ok: false,
                error: ErrorBody {
                    code: self.code,
                    message: self.message,
                    request_id: format!(
                        "eng-{:016x}",
                        REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
                    ),
                    retryable: self.retryable,
                },
            }),
        )
            .into_response()
    }
}

/// Build the setup UI, controlled write routes, and existing workbench handoff.
///
/// # Errors
///
/// Fails closed when a policy, schema, object pack, workspace, or production
/// asset cannot be fixed and validated at process startup.
#[allow(clippy::too_many_lines)]
pub fn build_engagement_application_router(
    config: EngagementWorkbenchConfig,
    assets_root: impl AsRef<Path>,
) -> Result<Router> {
    build_engagement_application_router_with_runtime_identity(config, assets_root, None)
}

/// Build the engagement application with one token-authenticated identity
/// endpoint for the verified, session-local assisted runtime supervisor.
///
/// # Errors
///
/// Fails closed when the supplied runtime session or workspace identity is
/// malformed or when normal engagement startup validation fails.
#[allow(clippy::too_many_lines)]
pub fn build_engagement_application_router_with_runtime_identity(
    config: EngagementWorkbenchConfig,
    assets_root: impl AsRef<Path>,
    runtime_identity_config: Option<&AssistedRuntimeIdentityConfig>,
) -> Result<Router> {
    let intake_policy = IntakePolicy::from_path(Some(&config.intake_policy))?;
    let profiling_policy = ProfilingPolicy::from_path(&config.profiling_policy)?;
    let validation_policy = ValidationPolicy::from_path(&config.validation_policy)?;
    let object_packs = load_object_packs(&config.object_packs, &config.profile_schema)?;
    let maximum_source_bytes = intake_policy.maximum_source_bytes;
    let profile_schema = config.profile_schema.clone();
    let service = EngagementService::new(EngagementServiceConfig {
        workspace: config.workspace,
        intake_policy,
        profiling_policy,
        profile_schema_path: config.profile_schema,
        mapping_schema_path: config.mapping_schema,
        validation_policy,
        object_packs,
    })?;
    if service.context()?.engagement.is_some() {
        // A reopened local process first reconciles bounded kernel attempts and
        // engagement pointers. This cannot create authority or select a new
        // source/profile/mapping; it only closes interrupted durable work.
        service.recover()?;
    }
    let service = Arc::new(service);
    let assisted_runtime_identity_state = runtime_identity_config
        .map(|runtime| prepare_assisted_runtime_identity(&service, runtime))
        .transpose()?;
    let state = EngagementWorkbenchState {
        service,
        assets: StaticAssets::load(assets_root.as_ref())?,
        maximum_source_bytes,
        profile_schema,
        assisted_runtime_identity: assisted_runtime_identity_state,
        downstream_router: Arc::new(tokio::sync::Mutex::new(None)),
    };
    Ok(Router::new()
        .route(
            "/__migration-stream/runtime-identity",
            get(assisted_runtime_identity),
        )
        .route("/api/v1/object-workspace", get(object_workspace_status))
        .route(
            "/api/v1/object-workspace/initialize",
            post(initialize_object_workspace),
        )
        .route(
            "/api/v1/object-workspace/adopt",
            post(adopt_object_workspace),
        )
        .route(
            "/api/v1/engagement",
            get(engagement_detail).post(create_engagement),
        )
        .route("/api/v1/journey", get(engagement_journey))
        .route("/api/v1/engagement/destination", get(destination_workshop))
        .route(
            "/api/v1/engagement/destination/edmx-imports",
            post(reserve_destination_edmx),
        )
        .route(
            "/api/v1/engagement/destination/edmx-imports/{attempt_id}/content",
            put(upload_destination_edmx),
        )
        .route(
            "/api/v1/engagement/destination/manual-catalogues",
            post(create_manual_destination_catalogue),
        )
        .route(
            "/api/v1/engagement/destination/catalogue/entities",
            get(destination_catalogue_entities),
        )
        .route(
            "/api/v1/engagement/destination/catalogue/properties",
            get(destination_catalogue_properties),
        )
        .route(
            "/api/v1/engagement/destination/drafts/start",
            post(start_destination_draft),
        )
        .route(
            "/api/v1/engagement/destination/drafts/save",
            post(save_destination_draft),
        )
        .route(
            "/api/v1/engagement/destination/drafts/rebase",
            post(rebase_destination_draft),
        )
        .route(
            "/api/v1/engagement/destination/drafts/freeze",
            post(freeze_destination_draft),
        )
        .route(
            "/api/v1/engagement/destination/profiles/approve",
            post(approve_destination_profile),
        )
        .route(
            "/api/v1/engagement/destination/mappings/carry-forward",
            post(carry_forward_destination_mapping),
        )
        .route(
            "/api/v1/engagement/source-preflights",
            post(reserve_source_preflight),
        )
        .route(
            "/api/v1/engagement/source-preflights/{preflight_id}/content",
            put(upload_source_preflight),
        )
        .route(
            "/api/v1/engagement/source-preflights/{preflight_id}/accept",
            post(accept_source_preflight),
        )
        .route(
            "/api/v1/engagement/object-packs",
            get(available_object_packs),
        )
        .route(
            "/api/v1/engagement/object-packs/select",
            post(select_object_pack),
        )
        .route(
            "/api/v1/engagement/object-packs/approve",
            post(approve_object_pack_profile),
        )
        .route(
            "/api/v1/engagement/source-profile-rules",
            get(source_profile_rules),
        )
        .route(
            "/api/v1/engagement/mapping-coverage",
            get(active_mapping_coverage),
        )
        .route(
            "/api/v1/engagement/mapping-workspace",
            get(mapping_workspace),
        )
        .route(
            "/api/v1/engagement/mapping-contract-coverage",
            get(mapping_contract_coverage),
        )
        .route(
            "/api/v1/engagement/mapping-drafts/save",
            post(save_mapping_draft),
        )
        .route(
            "/api/v1/engagement/mapping-drafts/submit",
            post(submit_mapping_draft),
        )
        .route(
            "/api/v1/engagement/mappings/approve",
            post(approve_mapping_version),
        )
        .route("/api/v1/engagement/mappings/prepare", post(prepare_mapping))
        .route(
            "/api/v1/engagement/validation/launch",
            post(launch_validation),
        )
        .route("/api/v1/engagement/tours", get(engagement_tours))
        .route("/api/v1/engagement/tours/start", post(start_revised_tour))
        .route(
            "/api/v1/engagement/tours/revised-source-preflights",
            post(reserve_revised_source_preflight),
        )
        .route(
            "/api/v1/engagement/tours/revised-source-preflights/{preflight_id}/accept",
            post(accept_revised_source_preflight),
        )
        .route(
            "/api/v1/engagement/tours/validation/launch",
            post(launch_revised_validation),
        )
        .route(
            "/api/v1/engagement/remediation-packs/options",
            get(remediation_pack_options),
        )
        .route(
            "/api/v1/engagement/remediation-packs",
            get(remediation_pack_history).post(create_remediation_pack),
        )
        .route(
            "/api/v1/engagement/remediation-packs/{pack_id}/download",
            get(download_remediation_pack),
        )
        .route(
            "/api/v1/engagement/readiness-packages",
            get(engagement_readiness_packages),
        )
        .route(
            "/api/v1/engagement/readiness-packages/{publication_id}",
            get(engagement_readiness_package),
        )
        .route(
            "/api/v1/engagement/readiness-packages/{publication_id}/download",
            get(download_engagement_readiness_package),
        )
        .fallback(engagement_fallback)
        .layer(middleware::from_fn(enforce_transport_bounds))
        .layer(middleware::from_fn(add_security_headers))
        .with_state(state))
}

fn prepare_assisted_runtime_identity(
    service: &EngagementService,
    runtime: &AssistedRuntimeIdentityConfig,
) -> Result<AssistedRuntimeIdentityState> {
    if !assisted_runtime_session_token_is_valid(&runtime.session_token) {
        return Err(KernelError::RuntimeUnavailable(
            "assisted runtime session token is invalid".to_owned(),
        ));
    }
    let listen = runtime
        .listen_address
        .parse::<std::net::SocketAddr>()
        .map_err(|_| {
            KernelError::RuntimeUnavailable(
                "assisted runtime listen identity is invalid".to_owned(),
            )
        })?;
    if !listen.ip().is_loopback() {
        return Err(KernelError::RuntimeUnavailable(
            "assisted runtime listen identity is not loopback".to_owned(),
        ));
    }
    let workspace_path = service
        .workspace()
        .canonicalize()
        .map_err(|error| KernelError::io(service.workspace(), error))?;
    let metadata = fs::symlink_metadata(&workspace_path)
        .map_err(|error| KernelError::io(&workspace_path, error))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(KernelError::RuntimeUnavailable(
            "assisted runtime workspace identity is not one real directory".to_owned(),
        ));
    }
    let filesystem = runtime_filesystem_identity(&workspace_path, &metadata)?;
    Ok(AssistedRuntimeIdentityState {
        session_token_sha256: format!("{:x}", Sha256::digest(runtime.session_token.as_bytes())),
        listen_address: listen.to_string(),
        process_id: std::process::id(),
        workspace_path: filesystem.canonical_path,
        filesystem_identity_sha256: filesystem.filesystem_identity_sha256,
    })
}

/// Validate the platform-native private runtime token without accepting an
/// alternate case, prefix, whitespace, or variable-length representation.
/// Unix retains its existing 128-bit token contract; Windows uses the full
/// 256-bit CSPRNG capability owned by the protected named-pipe session.
#[must_use]
pub fn assisted_runtime_session_token_is_valid(value: &str) -> bool {
    #[cfg(unix)]
    const EXPECTED_HEX_LENGTH: usize = 32;
    #[cfg(windows)]
    const EXPECTED_HEX_LENGTH: usize = 64;
    #[cfg(not(any(unix, windows)))]
    const EXPECTED_HEX_LENGTH: usize = 0;

    lowercase_hex_token_has_exact_length(value, EXPECTED_HEX_LENGTH)
}

fn lowercase_hex_token_has_exact_length(value: &str, expected_length: usize) -> bool {
    expected_length != 0
        && value.len() == expected_length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)]
fn runtime_filesystem_identity(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<RuntimeFilesystemIdentity> {
    let filesystem_device = metadata.dev();
    let filesystem_inode = metadata.ino();
    Ok(RuntimeFilesystemIdentity {
        canonical_path: path.to_path_buf(),
        filesystem_identity_sha256:
            crate::runtime_lifecycle::unix_workspace_filesystem_identity_sha256(
                filesystem_device,
                filesystem_inode,
            ),
    })
}

#[cfg(windows)]
fn runtime_filesystem_identity(
    path: &Path,
    _metadata: &fs::Metadata,
) -> Result<RuntimeFilesystemIdentity> {
    use migration_stream_windows_native::RealDirectory;

    let directory = RealDirectory::capture(path).map_err(|error| {
        KernelError::RuntimeUnavailable(format!(
            "Windows assisted-runtime workspace identity is unavailable: {error}"
        ))
    })?;
    directory.verify().map_err(|error| {
        KernelError::RuntimeUnavailable(format!(
            "Windows assisted-runtime workspace identity changed: {error}"
        ))
    })?;
    let identity = directory.identity();
    Ok(RuntimeFilesystemIdentity {
        canonical_path: directory.path().to_path_buf(),
        filesystem_identity_sha256:
            migration_stream_windows_native::windows_workspace_filesystem_identity_sha256(identity),
    })
}

#[cfg(not(any(unix, windows)))]
fn runtime_filesystem_identity(
    _path: &Path,
    _metadata: &fs::Metadata,
) -> Result<RuntimeFilesystemIdentity> {
    Err(KernelError::RuntimeUnavailable(
        "the assisted runtime identity is supported only on Unix hosts".to_owned(),
    ))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AssistedRuntimeIdentityResponse {
    schema_version: &'static str,
    session_token_sha256: String,
    process_id: u32,
    listen_address: String,
    workspace_path: PathBuf,
    filesystem_identity_sha256: String,
    application_identity_sha256: String,
}

async fn assisted_runtime_identity(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if uri.query().is_some() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(runtime) = state.assisted_runtime_identity.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let presented_token_sha256 = headers
        .get(ASSISTED_RUNTIME_IDENTITY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(|value| format!("{:x}", Sha256::digest(value.as_bytes())));
    if presented_token_sha256.as_deref() != Some(runtime.session_token_sha256.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let service = Arc::clone(&state.service);
    match tokio::task::spawn_blocking(move || {
        let first = assisted_runtime_authority(&service)?;
        let second = assisted_runtime_authority(&service)?;
        if first != second {
            return Err(KernelError::RuntimeUnavailable(
                "application authority changed during the runtime identity read".to_owned(),
            ));
        }
        let identity = AssistedRuntimeIdentityResponse {
            schema_version: ASSISTED_RUNTIME_IDENTITY_SCHEMA,
            session_token_sha256: runtime.session_token_sha256,
            process_id: runtime.process_id,
            listen_address: runtime.listen_address,
            workspace_path: runtime.workspace_path,
            filesystem_identity_sha256: runtime.filesystem_identity_sha256,
            application_identity_sha256: canonical_hash(&first)?,
        };
        canonical_json_bytes(&serde_json::to_value(identity)?)
    })
    .await
    {
        Ok(Ok(bytes)) => {
            let mut response = Response::new(Body::from(bytes));
            response
                .headers_mut()
                .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            response
        }
        Ok(Err(error)) => ApiError::from_kernel(&error).into_response(),
        Err(_) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "RUNTIME_IDENTITY_UNAVAILABLE",
            "The bounded local runtime identity is unavailable.",
            false,
        )
        .into_response(),
    }
}

fn assisted_runtime_authority(service: &EngagementService) -> Result<serde_json::Value> {
    let journey = service.journey()?;
    let page = PageRequest {
        page_size: 1,
        cursor: None,
    };
    let (snapshot, candidate, action, publications) =
        if let Some(job_id) = journey.binding.job_id.as_deref() {
            let reconciliation = ReconciliationReadService::new(service.workspace());
            let snapshot = reconciliation
                .snapshot_list(job_id, &page)?
                .items
                .into_iter()
                .next();
            let candidate = reconciliation
                .candidate_list(job_id, &page)?
                .items
                .into_iter()
                .next();
            let action = reconciliation
                .action_history(job_id, None, &page)?
                .items
                .into_iter()
                .next();
            let publications =
                PublicationReadService::new(service.workspace()).authority_head(job_id)?;
            (snapshot, candidate, action, Some(publications))
        } else {
            (None, None, None, None)
        };
    let workspace = narrow_runtime_workspace_authority(&journey.workspace);
    let snapshot = snapshot.as_ref().map(narrow_runtime_snapshot_head);
    let candidate = candidate.as_ref().map(narrow_runtime_candidate_head);
    let action = action.as_ref().map(narrow_runtime_action_head);
    let publications = publications.map(narrow_runtime_publication_head);
    Ok(serde_json::json!({
        "schemaVersion": ASSISTED_RUNTIME_IDENTITY_SCHEMA,
        "workspace": workspace,
        "journeyBinding": journey.binding,
        "reconciliationSnapshotHead": snapshot,
        "resultCandidateHead": candidate,
        "resultActionHead": action,
        "publicationAuthorityHead": publications,
    }))
}

fn narrow_runtime_workspace_authority(
    value: &crate::object_workspace::ObjectWorkspaceStatus,
) -> serde_json::Value {
    serde_json::json!({
        "state": value.state,
        "stateHash": value.state_hash,
        "requiredAction": value.required_action,
        "workspaceId": value.workspace.as_ref().map(|workspace| &workspace.workspace_id),
        "workspaceGeneration": value.workspace.as_ref().map(|workspace| workspace.generation),
        "descriptorHash": value.workspace.as_ref().map(|workspace| &workspace.descriptor_hash),
    })
}

fn narrow_runtime_snapshot_head(
    value: &crate::reconciliation_read_model::ResultReconciliationSnapshotSummary,
) -> serde_json::Value {
    serde_json::json!({
        "snapshotId": value.snapshot_id,
        "state": value.state,
        "jobId": value.job_id,
        "acceptedRowCount": value.accepted_row_count,
        "governingInputHash": value.governing_input_hash,
        "snapshotHash": value.snapshot_hash,
        "current": value.current,
        "counts": value.counts,
        "warningCodes": value.warning_codes,
        "blockerCodes": value.blocker_codes,
    })
}

fn narrow_runtime_candidate_head(
    value: &crate::reconciliation_read_model::ResultCandidateSummary,
) -> serde_json::Value {
    serde_json::json!({
        "candidateId": value.candidate_id,
        "snapshotId": value.snapshot_id,
        "snapshotHash": value.snapshot_hash,
        "candidateHash": value.candidate_hash,
        "current": value.current,
        "eligible": value.eligible,
        "includedCount": value.included_count,
        "excludedCount": value.excluded_count,
        "blockedCount": value.blocked_count,
        "warningCodes": value.warning_codes,
        "requiredAcknowledgments": value.required_acknowledgments,
    })
}

fn narrow_runtime_action_head(
    value: &crate::reconciliation_read_model::ResultActionSummary,
) -> serde_json::Value {
    serde_json::json!({
        "actionId": value.action_id,
        "actionKind": value.action_kind,
        "actionHash": value.action_hash,
        "candidateId": value.candidate_id,
        "candidateHash": value.candidate_hash,
        "generation": value.generation,
        "predecessorActionId": value.predecessor_action_id,
    })
}

fn narrow_runtime_publication_head(
    value: crate::publication_read_model::PublicationAuthorityHead,
) -> serde_json::Value {
    serde_json::json!({
        "totalCount": value.total_count,
        "latest": value.latest.map(|latest| serde_json::json!({
            "publicationId": latest.publication_id,
            "jobId": latest.job_id,
            "candidateId": latest.candidate_id,
            "approvalId": latest.approval_id,
            "status": latest.status,
            "semanticPublicationHash": latest.semantic_publication_hash,
            "physicalArchiveDigest": latest.physical_archive_digest,
            "packageSizeBytes": latest.package_size_bytes,
        })),
    })
}

fn load_object_packs(root: &Path, profile_schema: &Path) -> Result<Vec<ControlledObjectPack>> {
    require_real_directory(root, "object-pack catalogue")?;
    let canonical_root = root
        .canonicalize()
        .map_err(|error| KernelError::io(root, error))?;
    let mut packs = Vec::new();
    for pack_entry in sorted_directory_entries(&canonical_root)? {
        let pack_path = pack_entry.path();
        require_real_directory(&pack_path, "object-pack directory")?;
        for version_entry in sorted_directory_entries(&pack_path)? {
            if packs.len() >= MAXIMUM_OBJECT_PACKS {
                return Err(KernelError::PolicyInvalid(
                    "object-pack catalogue exceeds its pack-count bound".to_owned(),
                ));
            }
            let version_path = version_entry.path();
            require_real_directory(&version_path, "object-pack version directory")?;
            let manifest_path = version_path.join("manifest.json");
            let metadata = fs::symlink_metadata(&manifest_path)
                .map_err(|error| KernelError::io(&manifest_path, error))?;
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Err(KernelError::PolicyInvalid(
                    "each object-pack version requires one real manifest.json".to_owned(),
                ));
            }
            let length = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
            if length == 0 || length > MAXIMUM_MANIFEST_BYTES {
                return Err(KernelError::PolicyInvalid(
                    "object-pack manifest exceeds its byte bound".to_owned(),
                ));
            }
            let bytes =
                fs::read(&manifest_path).map_err(|error| KernelError::io(&manifest_path, error))?;
            if bytes.len() != length {
                return Err(KernelError::IntegrityFailed(
                    "object-pack manifest identity changed while loading".to_owned(),
                ));
            }
            let value = parse_unique_json(&bytes)
                .map_err(|error| KernelError::PolicyInvalid(error.to_string()))?;
            let manifest: ObjectPackManifest = serde_json::from_value(value)
                .map_err(|error| KernelError::PolicyInvalid(error.to_string()))?;
            packs.push(compile_object_pack(
                &canonical_root,
                &pack_path,
                &version_path,
                manifest,
                profile_schema,
            )?);
        }
    }
    if packs.is_empty() {
        return Err(KernelError::PolicyInvalid(
            "object-pack catalogue is empty".to_owned(),
        ));
    }
    Ok(packs)
}

#[allow(clippy::too_many_lines)]
fn compile_object_pack(
    root: &Path,
    pack_path: &Path,
    version_path: &Path,
    manifest: ObjectPackManifest,
    profile_schema: &Path,
) -> Result<ControlledObjectPack> {
    if manifest.schema_version != 1 {
        return Err(KernelError::PolicyInvalid(
            "unsupported object-pack manifest schema version".to_owned(),
        ));
    }
    let directory_pack_id = pack_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| KernelError::PolicyInvalid("object-pack path is not UTF-8".to_owned()))?;
    let directory_version = version_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| KernelError::PolicyInvalid("object-pack version is not UTF-8".to_owned()))?;
    if manifest.pack_id != directory_pack_id || manifest.version != directory_version {
        return Err(KernelError::IntegrityFailed(
            "object-pack manifest identity does not match its controlled directory".to_owned(),
        ));
    }
    let mut components = Path::new(&manifest.profile_file).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(KernelError::PolicyInvalid(
            "object-pack profileFile must be one controlled filename".to_owned(),
        ));
    }
    let profile_path = version_path.join(&manifest.profile_file);
    let profile_metadata = fs::symlink_metadata(&profile_path)
        .map_err(|error| KernelError::io(&profile_path, error))?;
    if !profile_metadata.file_type().is_file() || profile_metadata.file_type().is_symlink() {
        return Err(KernelError::PolicyInvalid(
            "object-pack profile must be one real regular file".to_owned(),
        ));
    }
    let canonical_profile = profile_path
        .canonicalize()
        .map_err(|error| KernelError::io(&profile_path, error))?;
    if !canonical_profile.starts_with(root) {
        return Err(KernelError::IntegrityFailed(
            "object-pack profile escaped its controlled catalogue".to_owned(),
        ));
    }
    let validated = validate_profile(profile_schema, &canonical_profile)?;
    let summary = validated.summary();
    if summary.status != "REVIEWED_DRAFT" {
        return Err(KernelError::ProfileNotApproved(format!(
            "controlled object-pack profile must be REVIEWED_DRAFT; found {}",
            summary.status
        )));
    }
    let profile_hash = canonical_hash(validated.document())?;
    if profile_hash != manifest.profile_sha256 {
        return Err(KernelError::IntegrityFailed(
            "object-pack profile canonical hash differs from its manifest".to_owned(),
        ));
    }
    if manifest.title.trim().is_empty()
        || manifest.scope.trim().is_empty()
        || manifest.limitations.is_empty()
        || (manifest.mapping_mode == MappingMode::ControlledSuggestions
            && manifest.mapping_suggestions.is_empty())
        || (manifest.mapping_mode == MappingMode::OperatorGoverned
            && !manifest.mapping_suggestions.is_empty())
    {
        return Err(KernelError::PolicyInvalid(
            "object-pack commercial scope, limits, and mapping mode must be explicit".to_owned(),
        ));
    }
    if manifest
        .limitations
        .iter()
        .any(|limitation| limitation.trim().is_empty() || limitation.len() > 1_024)
    {
        return Err(KernelError::PolicyInvalid(
            "object-pack limitations must be bounded nonempty statements".to_owned(),
        ));
    }
    let mut reference_table_ids = HashSet::new();
    for table in &manifest.reference_tables {
        let table_entity = table
            .entity_id
            .as_deref()
            .unwrap_or(&manifest.root_entity_id);
        if !safe_identifier(&table.table_id)
            || !safe_identifier(table_entity)
            || !reference_table_ids.insert((table_entity, table.table_id.as_str()))
            || table.values.is_empty()
            || table.values.len() > 10_000
            || table.values.iter().any(|(source, target)| {
                source.is_empty()
                    || source.len() > 4_096
                    || target.len() > 4_096
                    || source.chars().any(char::is_control)
                    || target.chars().any(char::is_control)
            })
        {
            return Err(KernelError::PolicyInvalid(
                "object-pack reference tables exceed their controlled local contract".to_owned(),
            ));
        }
    }
    let mut suggestion_ids = HashSet::new();
    let mut suggestion_targets = HashSet::new();
    let mapping_suggestions = manifest
        .mapping_suggestions
        .into_iter()
        .map(|suggestion| {
            let target_entity = suggestion
                .target_entity_id
                .as_deref()
                .unwrap_or(&manifest.root_entity_id);
            if !safe_identifier(&suggestion.suggestion_id)
                || !safe_identifier(target_entity)
                || !safe_identifier(&suggestion.target_field_id)
                || !suggestion_ids.insert(suggestion.suggestion_id.clone())
                || !suggestion_targets
                    .insert((target_entity.to_owned(), suggestion.target_field_id.clone()))
            {
                return Err(KernelError::PolicyInvalid(
                    "object-pack suggestion and target IDs must be safe and unique".to_owned(),
                ));
            }
            validate_manifest_suggestion(&suggestion, target_entity, &reference_table_ids)?;
            Ok(ObjectPackMappingSuggestion {
                suggestion_id: suggestion.suggestion_id,
                target_entity_id: suggestion.target_entity_id,
                target_field_id: suggestion.target_field_id,
                source_file: suggestion.source_file,
                source_column: suggestion.source_column,
                source_column_ordinal: suggestion.source_column_ordinal,
                kind: suggestion.kind,
                constant: suggestion.constant,
                lookup_table_id: suggestion.lookup_table_id,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    drop(reference_table_ids);
    Ok(ControlledObjectPack {
        object_pack_id: manifest.pack_id,
        version: manifest.version,
        title: manifest.title,
        description: manifest.scope.clone(),
        scope: manifest.scope,
        root_entity_id: manifest.root_entity_id,
        mapping_mode: manifest.mapping_mode,
        limitations: manifest.limitations,
        mapping_suggestions,
        reference_tables: manifest.reference_tables,
        profile_document: validated.document().clone(),
    })
}

fn validate_manifest_suggestion(
    suggestion: &ManifestMappingSuggestion,
    target_entity: &str,
    reference_table_ids: &HashSet<(&str, &str)>,
) -> Result<()> {
    let has_source = match (
        suggestion.source_file.as_deref(),
        suggestion.source_column.as_deref(),
        suggestion.source_column_ordinal,
    ) {
        (Some(file), Some(column), Some(ordinal))
            if safe_source_file(file)
                && !column.is_empty()
                && column.len() <= 1_024
                && !column.chars().any(char::is_control)
                && ordinal < 4_096 =>
        {
            true
        }
        (None, None, None) => false,
        _ => {
            return Err(KernelError::PolicyInvalid(
                "object-pack suggestion has an incomplete or unsafe source identity".to_owned(),
            ));
        }
    };
    match suggestion.kind {
        MappingKind::Constant => {
            if has_source
                || suggestion.constant.as_deref().is_none_or(|constant| {
                    constant.len() > 4_096 || constant.chars().any(char::is_control)
                })
                || suggestion.lookup_table_id.is_some()
            {
                return Err(KernelError::PolicyInvalid(
                    "constant object-pack suggestion has inconsistent parameters".to_owned(),
                ));
            }
        }
        MappingKind::Lookup => {
            if !has_source
                || suggestion.constant.is_some()
                || suggestion
                    .lookup_table_id
                    .as_deref()
                    .is_none_or(|table_id| {
                        !safe_identifier(table_id)
                            || !reference_table_ids.contains(&(target_entity, table_id))
                    })
            {
                return Err(KernelError::PolicyInvalid(
                    "lookup object-pack suggestion lacks exact controlled reference data"
                        .to_owned(),
                ));
            }
        }
        _ => {
            if !has_source || suggestion.constant.is_some() || suggestion.lookup_table_id.is_some()
            {
                return Err(KernelError::PolicyInvalid(
                    "source object-pack suggestion has inconsistent parameters".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

fn safe_source_file(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1_024
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn require_real_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| KernelError::io(path, error))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(KernelError::PolicyInvalid(format!(
            "{label} must be one real non-link directory"
        )));
    }
    Ok(())
}

fn sorted_directory_entries(path: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| KernelError::io(path, error))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| KernelError::io(path, error))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

async fn engagement_detail(State(state): State<EngagementWorkbenchState>, uri: Uri) -> Response {
    if uri.query().is_some() {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.detail()
    })
    .await
}

async fn object_workspace_status(
    State(state): State<EngagementWorkbenchState>,
    uri: Uri,
) -> Response {
    if uri.query().is_some() {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.object_workspace_status()
    })
    .await
}

async fn initialize_object_workspace(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key = match validate_command_headers(
        &headers,
        INITIALIZE_OBJECT_WORKSPACE_INTENT,
        "application/json",
    ) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let command: EstablishObjectWorkspaceCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.initialize_object_workspace(&command)
    })
    .await
}

async fn adopt_object_workspace(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, ADOPT_OBJECT_WORKSPACE_INTENT, "application/json")
        {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: EstablishObjectWorkspaceCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != ADOPT_OBJECT_WORKSPACE_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.adopt_object_workspace(&command)
    })
    .await
}

async fn create_engagement(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, CREATE_INTENT, "application/json") {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: CreateOrReopenCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != CREATE_ENGAGEMENT_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.create_or_reopen(&command)
    })
    .await
}

async fn destination_workshop(State(state): State<EngagementWorkbenchState>, uri: Uri) -> Response {
    if uri.query().is_some() {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.destination_service().workshop()
    })
    .await
}

async fn destination_catalogue_entities(
    State(state): State<EngagementWorkbenchState>,
    query: std::result::Result<Query<DestinationCatalogueEntitiesQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return ApiError::invalid_request().into_response();
    };
    run_domain({
        let service = Arc::clone(&state.service);
        move || {
            service.destination_service().catalogue_entities(
                &query.catalogue_authority_id,
                query.query.as_deref(),
                query.page_size.unwrap_or(50),
                query.cursor.as_deref(),
            )
        }
    })
    .await
}

async fn destination_catalogue_properties(
    State(state): State<EngagementWorkbenchState>,
    query: std::result::Result<Query<DestinationCataloguePropertiesQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return ApiError::invalid_request().into_response();
    };
    run_domain({
        let service = Arc::clone(&state.service);
        move || {
            service.destination_service().catalogue_properties(
                &query.catalogue_authority_id,
                &query.technical_entity,
                query.query.as_deref(),
                query.state,
                query.page_size.unwrap_or(50),
                query.cursor.as_deref(),
            )
        }
    })
    .await
}

async fn reserve_destination_edmx(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key = match validate_command_headers(
        &headers,
        DESTINATION_EDMX_RESERVE_INTENT,
        "application/json",
    ) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let command: ReserveEdmxCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != RESERVE_EDMX_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.destination_service().reserve_edmx(&command)
    })
    .await
}

#[allow(clippy::too_many_lines)]
async fn upload_destination_edmx(
    State(state): State<EngagementWorkbenchState>,
    AxumPath(attempt_id): AxumPath<String>,
    headers: HeaderMap,
    mut body: Body,
) -> Response {
    if !safe_identifier(&attempt_id) {
        return ApiError::invalid_request().into_response();
    }
    let idempotency_key = match validate_command_headers(
        &headers,
        DESTINATION_EDMX_CONTENT_INTENT,
        "application/xml",
    ) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Some(length) = content_length(&headers) {
        let Ok(length) = length else {
            return ApiError::invalid_transport().into_response();
        };
        if length == 0 || length > MAXIMUM_EDMX_BYTES {
            return ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "EDMX_SIZE",
                "The selected EDMX exceeds the local destination artifact limit.",
                false,
            )
            .into_response();
        }
    }
    let workspace_lock = match state.service.acquire_object_workspace_mutation_lock() {
        Ok(lock) => lock,
        Err(error) => return ApiError::from_kernel(&error).into_response(),
    };
    let opened = match state
        .service
        .destination_service()
        .open_edmx_upload(&attempt_id, &idempotency_key)
    {
        Ok(file) => file,
        Err(error) => {
            drop(workspace_lock);
            return ApiError::from_kernel(&error).into_response();
        }
    };
    let Ok(completion_file) = opened.try_clone() else {
        drop(opened);
        drop(workspace_lock);
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "EDMX_WRITE_UNAVAILABLE",
            "The reserved local EDMX file could not be retained.",
            false,
        )
        .into_response();
    };
    let mut file = tokio::fs::File::from_std(opened);
    let mut size = 0_u64;
    while let Some(frame_result) = body.frame().await {
        let Ok(frame) = frame_result else {
            drop(file);
            drop(workspace_lock);
            return terminate_destination_edmx(
                &state,
                &attempt_id,
                &idempotency_key,
                "UPLOAD_STREAM_INTERRUPTED",
                true,
            )
            .await;
        };
        let Ok(data) = frame.into_data() else {
            continue;
        };
        size = match size.checked_add(u64::try_from(data.len()).unwrap_or(u64::MAX)) {
            Some(size) if size <= MAXIMUM_EDMX_BYTES => size,
            _ => {
                drop(file);
                drop(workspace_lock);
                return terminate_destination_edmx(
                    &state,
                    &attempt_id,
                    &idempotency_key,
                    "EDMX_SIZE",
                    false,
                )
                .await;
            }
        };
        if file.write_all(&data).await.is_err() {
            drop(file);
            drop(workspace_lock);
            return terminate_destination_edmx(
                &state,
                &attempt_id,
                &idempotency_key,
                "UPLOAD_WRITE_FAILED",
                false,
            )
            .await;
        }
    }
    if size == 0 || file.sync_all().await.is_err() {
        drop(file);
        drop(workspace_lock);
        return terminate_destination_edmx(
            &state,
            &attempt_id,
            &idempotency_key,
            "UPLOAD_WRITE_FAILED",
            false,
        )
        .await;
    }
    drop(file);
    if workspace_lock.verify().is_err() || state.service.object_workspace_status().is_err() {
        drop(workspace_lock);
        return terminate_destination_edmx(
            &state,
            &attempt_id,
            &idempotency_key,
            "OBJECT_WORKSPACE_INTEGRITY",
            true,
        )
        .await;
    }
    drop(workspace_lock);
    let service = Arc::clone(&state.service);
    let attempt_for_task = attempt_id.clone();
    let key_for_task = idempotency_key.clone();
    match tokio::task::spawn_blocking(move || {
        service.destination_service().complete_edmx_upload(
            &attempt_for_task,
            &key_for_task,
            completion_file,
        )
    })
    .await
    {
        Ok(Ok(artifact)) => Json(artifact).into_response(),
        Ok(Err(error)) => {
            let code = error.code().to_owned();
            let response = ApiError::from_kernel(&error).into_response();
            let _ = terminate_destination_edmx(&state, &attempt_id, &idempotency_key, &code, false)
                .await;
            response
        }
        Err(_) => {
            terminate_destination_edmx(
                &state,
                &attempt_id,
                &idempotency_key,
                "EDMX_IMPORT_TASK_FAILED",
                true,
            )
            .await
        }
    }
}

async fn terminate_destination_edmx(
    state: &EngagementWorkbenchState,
    attempt_id: &str,
    idempotency_key: &str,
    code: &str,
    interrupted: bool,
) -> Response {
    let service = Arc::clone(&state.service);
    let attempt_id = attempt_id.to_owned();
    let idempotency_key = idempotency_key.to_owned();
    let code = code.chars().take(64).collect::<String>();
    let _ = tokio::task::spawn_blocking(move || {
        service.destination_service().fail_edmx_upload(
            &attempt_id,
            &idempotency_key,
            &code,
            interrupted,
        )
    })
    .await;
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "EDMX_IMPORT_REJECTED",
        "The selected EDMX could not complete the bounded offline destination import.",
        false,
    )
    .into_response()
}

async fn create_manual_destination_catalogue(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, DESTINATION_MANUAL_INTENT, "application/json") {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: CreateManualCatalogueCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != CREATE_MANUAL_CATALOGUE_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || {
            service
                .destination_service()
                .create_manual_catalogue(&command)
        }
    })
    .await
}

async fn start_destination_draft(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key = match validate_command_headers(
        &headers,
        DESTINATION_DRAFT_START_INTENT,
        "application/json",
    ) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let command: StartDestinationDraftCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != START_DRAFT_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.destination_service().start_draft(&command)
    })
    .await
}

async fn save_destination_draft(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, DESTINATION_DRAFT_SAVE_INTENT, "application/json")
        {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: SaveDestinationDraftCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != SAVE_DRAFT_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.destination_service().save_draft(&command)
    })
    .await
}

async fn rebase_destination_draft(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key = match validate_command_headers(
        &headers,
        DESTINATION_DRAFT_REBASE_INTENT,
        "application/json",
    ) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let command: RebaseDestinationDraftCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != REBASE_DRAFT_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.destination_service().rebase_draft(&command)
    })
    .await
}

async fn freeze_destination_draft(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key = match validate_command_headers(
        &headers,
        DESTINATION_DRAFT_FREEZE_INTENT,
        "application/json",
    ) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let command: FreezeDestinationDraftCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != FREEZE_DRAFT_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.destination_service().freeze_draft(&command)
    })
    .await
}

async fn approve_destination_profile(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key = match validate_command_headers(
        &headers,
        DESTINATION_PROFILE_APPROVE_INTENT,
        "application/json",
    ) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let command: ApproveDestinationProfileCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != APPROVE_DESTINATION_PROFILE_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || {
            service
                .destination_service()
                .approve_destination_profile(&command)
        }
    })
    .await
}

async fn carry_forward_destination_mapping(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key = match validate_command_headers(
        &headers,
        DESTINATION_MAPPING_CARRY_FORWARD_INTENT,
        "application/json",
    ) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let command: CarryForwardMappingCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != CARRY_FORWARD_MAPPING_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || {
            service
                .destination_service()
                .carry_forward_mapping(&command)
        }
    })
    .await
}

async fn reserve_source_preflight(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, RESERVE_INTENT, "application/json") {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: ReservePreflightCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != RESERVE_PREFLIGHT_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.reserve_preflight(&command)
    })
    .await
}

#[allow(clippy::too_many_lines)]
async fn upload_source_preflight(
    State(state): State<EngagementWorkbenchState>,
    AxumPath(preflight_id): AxumPath<String>,
    headers: HeaderMap,
    mut body: Body,
) -> Response {
    if !safe_identifier(&preflight_id) {
        return ApiError::invalid_request().into_response();
    }
    let idempotency_key =
        match validate_command_headers(&headers, CONTENT_INTENT, "application/zip") {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    if let Some(length) = content_length(&headers) {
        let Ok(length) = length else {
            return ApiError::invalid_transport().into_response();
        };
        if length == 0 || length > state.maximum_source_bytes {
            return ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "SOURCE_SIZE",
                "The selected ZIP exceeds the configured local source-size limit.",
                false,
            )
            .into_response();
        }
    }
    if let Err(error) = state
        .service
        .require_preflight_upload_identity(&preflight_id, &idempotency_key)
    {
        return ApiError::from_kernel(&error).into_response();
    }

    let workspace_lock = match state.service.acquire_object_workspace_mutation_lock() {
        Ok(lock) => lock,
        Err(error) => return ApiError::from_kernel(&error).into_response(),
    };

    let opened = match state
        .service
        .open_preflight_upload(&preflight_id, &idempotency_key)
    {
        Ok(file) => file,
        Err(error) => {
            drop(workspace_lock);
            return terminate_stream(
                &state,
                &preflight_id,
                error.code(),
                "the reserved local upload identity failed closed",
                None,
            )
            .await;
        }
    };
    let Ok(expected_file) = opened.try_clone() else {
        drop(workspace_lock);
        return ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "PREFLIGHT_WRITE_UNAVAILABLE",
            "The reserved local source file could not be retained.",
            false,
        )
        .into_response();
    };
    let mut file = tokio::fs::File::from_std(opened);
    let mut size = 0_u64;
    let mut digest = Sha256::new();
    while let Some(frame_result) = body.frame().await {
        let Ok(frame) = frame_result else {
            drop(file);
            drop(workspace_lock);
            return terminate_stream(
                &state,
                &preflight_id,
                "UPLOAD_STREAM_INTERRUPTED",
                "the browser upload stream ended unexpectedly",
                Some(expected_file),
            )
            .await;
        };
        let Ok(data) = frame.into_data() else {
            continue;
        };
        size = match size.checked_add(u64::try_from(data.len()).unwrap_or(u64::MAX)) {
            Some(size) if size <= state.maximum_source_bytes => size,
            _ => {
                drop(file);
                drop(workspace_lock);
                return terminate_stream(
                    &state,
                    &preflight_id,
                    "SOURCE_SIZE",
                    "the source exceeded its configured upload bound",
                    Some(expected_file),
                )
                .await;
            }
        };
        digest.update(&data);
        if file.write_all(&data).await.is_err() {
            drop(file);
            drop(workspace_lock);
            return terminate_stream(
                &state,
                &preflight_id,
                "UPLOAD_WRITE_FAILED",
                "the controlled local source write failed",
                Some(expected_file),
            )
            .await;
        }
    }
    if size == 0 || file.sync_all().await.is_err() {
        drop(file);
        drop(workspace_lock);
        return terminate_stream(
            &state,
            &preflight_id,
            "UPLOAD_WRITE_FAILED",
            "the controlled local source write was empty or could not be synchronized",
            Some(expected_file),
        )
        .await;
    }
    drop(file);
    if workspace_lock.verify().is_err() || state.service.object_workspace_status().is_err() {
        drop(workspace_lock);
        return terminate_stream(
            &state,
            &preflight_id,
            "OBJECT_WORKSPACE_INTEGRITY",
            "the object workspace identity changed during local upload",
            Some(expected_file),
        )
        .await;
    }
    drop(workspace_lock);
    let sha256 = format!("{:x}", digest.finalize());
    let command = CompletePreflightCommand {
        preflight_id: preflight_id.clone(),
        sha256,
        size_bytes: size,
    };
    let Ok(completion_file) = expected_file.try_clone() else {
        return terminate_stream(
            &state,
            &preflight_id,
            "PREFLIGHT_IDENTITY_UNAVAILABLE",
            "the controlled local source identity could not be retained for completion",
            Some(expected_file),
        )
        .await;
    };
    let service = Arc::clone(&state.service);
    match tokio::task::spawn_blocking(move || {
        service.complete_preflight_upload(&command, &completion_file)
    })
    .await
    {
        Ok(Ok(preflight)) => Json(preflight).into_response(),
        Ok(Err(error)) => {
            let code = error.code().to_owned();
            let response = ApiError::from_kernel(&error).into_response();
            let _ = terminate_stream(
                &state,
                &preflight_id,
                &code,
                "the bounded archive preflight rejected the selected source",
                Some(expected_file),
            )
            .await;
            response
        }
        Err(_) => {
            terminate_stream(
                &state,
                &preflight_id,
                "PREFLIGHT_TASK_FAILED",
                "the bounded archive preflight task did not complete",
                Some(expected_file),
            )
            .await
        }
    }
}

async fn terminate_stream(
    state: &EngagementWorkbenchState,
    preflight_id: &str,
    code: &str,
    detail: &str,
    expected_file: Option<fs::File>,
) -> Response {
    let command = PreflightTerminalCommand {
        preflight_id: preflight_id.to_owned(),
        error_code: code.chars().take(64).collect(),
        detail: detail.chars().take(512).collect(),
    };
    let service = Arc::clone(&state.service);
    let _ = tokio::task::spawn_blocking(move || {
        if let Some(file) = expected_file.as_ref() {
            service.fail_preflight_upload(&command, file)
        } else {
            service.fail_preflight(&command)
        }
    })
    .await;
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        code,
        "The selected ZIP could not complete the bounded local source preflight.",
        false,
    )
    .into_response()
}

async fn accept_source_preflight(
    State(state): State<EngagementWorkbenchState>,
    AxumPath(preflight_id): AxumPath<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, ACCEPT_INTENT, "application/json") {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: AcceptPreflightCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if !safe_identifier(&preflight_id)
        || command.schema_version != ACCEPT_SOURCE_COMMAND_SCHEMA
        || command.preflight_id != preflight_id
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.accept_preflight(&command)
    })
    .await
}

async fn available_object_packs(
    State(state): State<EngagementWorkbenchState>,
    uri: Uri,
) -> Response {
    if uri.query().is_some() {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.available_object_packs()
    })
    .await
}

async fn select_object_pack(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, SELECT_PACK_INTENT, "application/json") {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: SelectObjectPackCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != SELECT_OBJECT_PACK_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.select_object_pack(&command)
    })
    .await
}

async fn approve_object_pack_profile(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, APPROVE_PROFILE_INTENT, "application/json") {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: ApproveProfileCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != APPROVE_OBJECT_PACK_PROFILE_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.approve_profile(&command)
    })
    .await
}

async fn source_profile_rules(State(state): State<EngagementWorkbenchState>, uri: Uri) -> Response {
    if uri.query().is_some() {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.source_profile_rules()
    })
    .await
}

async fn active_mapping_coverage(
    State(state): State<EngagementWorkbenchState>,
    query: std::result::Result<Query<PageRequest>, QueryRejection>,
) -> Response {
    let Ok(Query(page)) = query else {
        return ApiError::invalid_request().into_response();
    };
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.active_mapping_coverage(&page)
    })
    .await
}

async fn mapping_workspace(State(state): State<EngagementWorkbenchState>, uri: Uri) -> Response {
    if uri.query().is_some() {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.mapping_workspace()
    })
    .await
}

async fn mapping_contract_coverage(
    State(state): State<EngagementWorkbenchState>,
    uri: Uri,
) -> Response {
    if uri.query().is_some() {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.mapping_contract_coverage()
    })
    .await
}

async fn save_mapping_draft(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, SAVE_MAPPING_DRAFT_INTENT, "application/json") {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: SaveMappingDraftCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != SAVE_MAPPING_DRAFT_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.save_mapping_draft(&command)
    })
    .await
}

async fn submit_mapping_draft(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, SUBMIT_MAPPING_DRAFT_INTENT, "application/json") {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: SubmitMappingDraftCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != SUBMIT_MAPPING_DRAFT_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.submit_mapping_draft(&command)
    })
    .await
}

async fn approve_mapping_version(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key = match validate_command_headers(
        &headers,
        APPROVE_MAPPING_VERSION_INTENT,
        "application/json",
    ) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let command: ApproveMappingVersionCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != APPROVE_MAPPING_VERSION_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.approve_mapping_version(&command)
    })
    .await
}

async fn prepare_mapping(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, PREPARE_MAPPING_INTENT, "application/json") {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: crate::engagement::PrepareMappingCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != PREPARE_MAPPING_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.prepare_mapping(&command)
    })
    .await
}

async fn launch_validation(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, LAUNCH_VALIDATION_INTENT, "application/json") {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: LaunchValidationCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != LAUNCH_VALIDATION_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.launch_validation(&command)
    })
    .await
}

async fn engagement_tours(State(state): State<EngagementWorkbenchState>) -> Response {
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.tour_chain()
    })
    .await
}

async fn engagement_journey(State(state): State<EngagementWorkbenchState>) -> Response {
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.journey()
    })
    .await
}

async fn start_revised_tour(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, START_REVISED_TOUR_INTENT, "application/json") {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: StartRevisedTourCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != START_REVISED_TOUR_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.start_revised_tour(&command)
    })
    .await
}

async fn reserve_revised_source_preflight(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key =
        match validate_command_headers(&headers, RESERVE_REVISED_SOURCE_INTENT, "application/json")
        {
            Ok(value) => value,
            Err(error) => return error.into_response(),
        };
    let command: ReserveRevisedPreflightCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != RESERVE_REVISED_PREFLIGHT_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.reserve_revised_preflight(&command)
    })
    .await
}

async fn accept_revised_source_preflight(
    State(state): State<EngagementWorkbenchState>,
    AxumPath(preflight_id): AxumPath<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !safe_identifier(&preflight_id) {
        return ApiError::invalid_request().into_response();
    }
    let idempotency_key = match validate_command_headers(
        &headers,
        ACCEPT_REVISED_SOURCE_INTENT,
        "application/json",
    ) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let command: AcceptRevisedPreflightCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != ACCEPT_REVISED_SOURCE_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
        || command.preflight_id != preflight_id
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.accept_revised_preflight(&command)
    })
    .await
}

async fn launch_revised_validation(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key = match validate_command_headers(
        &headers,
        LAUNCH_REVISED_VALIDATION_INTENT,
        "application/json",
    ) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let command: LaunchRevisedValidationCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != LAUNCH_REVISED_VALIDATION_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.launch_revised_validation(&command)
    })
    .await
}

async fn engagement_readiness_packages(
    State(state): State<EngagementWorkbenchState>,
    uri: Uri,
) -> Response {
    if uri.query().is_some() {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || {
            let engagement = service.state()?;
            PublicationReadService::new(service.workspace())
                .list_for_engagement(&engagement.engagement_id)
        }
    })
    .await
}

async fn remediation_pack_options(
    State(state): State<EngagementWorkbenchState>,
    uri: Uri,
) -> Response {
    if uri.query().is_some() {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.remediation_pack_options()
    })
    .await
}

async fn remediation_pack_history(
    State(state): State<EngagementWorkbenchState>,
    uri: Uri,
) -> Response {
    if uri.query().is_some() {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.remediation_pack_history()
    })
    .await
}

async fn create_remediation_pack(
    State(state): State<EngagementWorkbenchState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let idempotency_key = match validate_command_headers(
        &headers,
        CREATE_REMEDIATION_PACK_INTENT,
        "application/json",
    ) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let command: CreateRemediationPackCommand = match decode_command(body).await {
        Ok(command) => command,
        Err(error) => return error.into_response(),
    };
    if command.schema_version != CREATE_REMEDIATION_PACK_COMMAND_SCHEMA
        || command.idempotency_key != idempotency_key
    {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || service.create_remediation_pack(&command)
    })
    .await
}

async fn download_remediation_pack(
    State(state): State<EngagementWorkbenchState>,
    AxumPath(pack_id): AxumPath<String>,
    uri: Uri,
) -> Response {
    if uri.query().is_some() || !safe_identifier(&pack_id) {
        return ApiError::invalid_request().into_response();
    }
    let service = Arc::clone(&state.service);
    let result =
        tokio::task::spawn_blocking(move || service.remediation_pack_download(&pack_id)).await;
    let (pack, bytes) = match result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return ApiError::from_kernel(&error).into_response(),
        Err(_) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "SETUP_TASK_FAILED",
                "The bounded local remediation-pack read did not complete safely.",
                false,
            )
            .into_response();
        }
    };
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/zip"),
    );
    let filename = format!("migration-stream-remediation-{}.zip", pack.pack_id);
    if let Ok(value) =
        axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
    {
        response
            .headers_mut()
            .insert(axum::http::header::CONTENT_DISPOSITION, value);
    }
    response
}

async fn engagement_readiness_package(
    State(state): State<EngagementWorkbenchState>,
    AxumPath(publication_id): AxumPath<String>,
    uri: Uri,
) -> Response {
    if uri.query().is_some() || !safe_identifier(&publication_id) {
        return ApiError::invalid_request().into_response();
    }
    run_domain({
        let service = Arc::clone(&state.service);
        move || {
            let engagement = service.state()?;
            PublicationReadService::new(service.workspace())
                .get_detail_for_engagement(&engagement.engagement_id, &publication_id)?
                .ok_or_else(|| KernelError::NotFound("historical readiness package".to_owned()))
        }
    })
    .await
}

async fn download_engagement_readiness_package(
    State(state): State<EngagementWorkbenchState>,
    AxumPath(publication_id): AxumPath<String>,
    uri: Uri,
) -> Response {
    if uri.query().is_some() || !safe_identifier(&publication_id) {
        return ApiError::invalid_request().into_response();
    }
    let service = Arc::clone(&state.service);
    let result = tokio::task::spawn_blocking(move || {
        let engagement = service.state()?;
        PublicationReadService::new(service.workspace())
            .read_archive_for_engagement(&engagement.engagement_id, &publication_id)?
            .ok_or_else(|| KernelError::NotFound("historical readiness package".to_owned()))
    })
    .await;
    let (record, bytes) = match result {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => return ApiError::from_kernel(&error).into_response(),
        Err(_) => {
            return ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "SETUP_TASK_FAILED",
                "The bounded local readiness-package read did not complete safely.",
                false,
            )
            .into_response();
        }
    };
    let mut response = Response::new(Body::from(bytes));
    response.headers_mut().insert(
        CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/zip"),
    );
    let filename = format!("migration-stream-readiness-{}.zip", record.publication_id);
    if let Ok(value) =
        axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
    {
        response
            .headers_mut()
            .insert(axum::http::header::CONTENT_DISPOSITION, value);
    }
    response
}

async fn engagement_fallback(
    State(state): State<EngagementWorkbenchState>,
    request: Request<Body>,
) -> Response {
    if !request.uri().path().starts_with("/api/") {
        return static_fallback(request.method(), request.uri(), &state.assets);
    }
    if request.uri().path() == "/api/v1/context" {
        let context = match state.service.context() {
            Ok(context) => context,
            Err(error) => return ApiError::from_kernel(&error).into_response(),
        };
        let authority_gated = state.service.multi_entity_authority_gated().unwrap_or(true);
        let completed = context.engagement.as_ref().is_some_and(|engagement| {
            engagement.validation_stage == "COMPLETE" && engagement.active.job_id.is_some()
        });
        if !completed || authority_gated {
            return Json(context).into_response();
        }
    }
    let (router, authority) = match cached_downstream_router(&state).await {
        Ok(Some(cached)) => cached,
        Ok(None) => {
            return ApiError::new(
                StatusCode::NOT_FOUND,
                "WORKBENCH_EVIDENCE_UNAVAILABLE",
                "Complete local source intake before opening practitioner evidence.",
                false,
            )
            .into_response();
        }
        Err(error) => return ApiError::from_kernel(&error).into_response(),
    };
    let response = match router.oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    };
    match downstream_router_authority(&state) {
        Ok((_, current)) if current == authority => response,
        Ok(_) => ApiError::from_kernel(&KernelError::IntegrityFailed(
            "active engagement authority changed while serving downstream evidence".to_owned(),
        ))
        .into_response(),
        Err(error) => ApiError::from_kernel(&error).into_response(),
    }
}

async fn cached_downstream_router(
    state: &EngagementWorkbenchState,
) -> Result<Option<(Router, DownstreamRouterAuthority)>> {
    let mut cached = state.downstream_router.lock().await;
    let (engagement, authority) = downstream_router_authority(state)?;
    if engagement.active.job_id.is_none() {
        return Ok(None);
    }
    if let Some(current) = cached.as_ref()
        && current.authority == authority
    {
        return Ok(Some((current.router.clone(), authority)));
    }
    let router = build_downstream_router(state, &engagement, &authority)?;
    let (_, confirmed) = downstream_router_authority(state)?;
    if confirmed != authority {
        return Err(KernelError::IntegrityFailed(
            "active engagement authority changed while preparing downstream evidence".to_owned(),
        ));
    }
    *cached = Some(CachedDownstreamRouter {
        authority: authority.clone(),
        router: router.clone(),
    });
    Ok(Some((router, authority)))
}

fn downstream_router_authority(
    state: &EngagementWorkbenchState,
) -> Result<(EngagementState, DownstreamRouterAuthority)> {
    let engagement = state.service.state()?;
    let multi_entity_authority_gated = state.service.multi_entity_authority_gated()?;
    let downstream_enabled = engagement.validation_stage == "COMPLETE"
        && engagement.active.job_id.is_some()
        && !multi_entity_authority_gated;
    let capabilities = if downstream_enabled {
        DownstreamCapabilityState::GovernedLocalReadiness
    } else if multi_entity_authority_gated {
        DownstreamCapabilityState::MultiEntityLocked
    } else {
        DownstreamCapabilityState::ReadOnly
    };
    let authority = DownstreamRouterAuthority {
        engagement_id: engagement.engagement_id.clone(),
        generation: engagement.generation,
        phase: engagement.phase.clone(),
        active: engagement.active.clone(),
        profile_stage: engagement.profile_stage.clone(),
        mapping_stage: engagement.mapping_stage.clone(),
        validation_stage: engagement.validation_stage.clone(),
        capabilities,
    };
    Ok((engagement, authority))
}

fn build_downstream_router(
    state: &EngagementWorkbenchState,
    engagement: &EngagementState,
    authority: &DownstreamRouterAuthority,
) -> Result<Router> {
    let expected_governance = match (
        engagement.active.profile_id.as_ref(),
        engagement.active.profile_version.as_ref(),
        engagement.active.profile_hash.as_ref(),
        engagement.active.mapping_id.as_ref(),
        engagement.active.mapping_version.as_ref(),
        engagement.active.mapping_hash.as_ref(),
        engagement.active.validation_run_id.as_ref(),
    ) {
        (_, _, _, _, _, _, None) => None,
        (
            Some(profile_id),
            Some(profile_version),
            Some(profile_hash),
            Some(mapping_id),
            Some(mapping_version),
            Some(mapping_hash),
            Some(validation_run_id),
        ) => Some(SelectedGovernance {
            profile_id: profile_id.clone(),
            profile_version: profile_version.clone(),
            profile_hash: profile_hash.clone(),
            mapping_id: mapping_id.clone(),
            mapping_version: mapping_version.clone(),
            mapping_hash: mapping_hash.clone(),
            validation_run_id: validation_run_id.clone(),
            validation_state: engagement.validation_stage.clone(),
        }),
        _ => {
            return Err(KernelError::IntegrityFailed(
                "active engagement validation authority is incomplete".to_owned(),
            ));
        }
    };
    let job_id = engagement.active.job_id.clone().ok_or_else(|| {
        KernelError::NotFound(
            "complete local source intake before opening practitioner evidence".to_owned(),
        )
    })?;
    let mut config = WorkbenchConfig::new(
        state.service.workspace(),
        job_id,
        engagement.active.profiling_run_id.clone(),
    )
    .with_expected_governance(expected_governance)
    .with_profile_schema(state.profile_schema.clone());
    if authority.capabilities == DownstreamCapabilityState::GovernedLocalReadiness {
        // Setup ends at first issues. From that exact completed validation
        // state, hand off to the already accepted cumulative local workflow:
        // governed decisions, reconciliation/result approval, and local
        // Readiness Packages. Target and external writes remain unavailable.
        config = config.with_readiness_publication();
    }
    build_router(config)
}

async fn decode_command<T: DeserializeOwned>(body: Body) -> std::result::Result<T, ApiError> {
    let bytes = to_bytes(body, MAXIMUM_COMMAND_BODY_BYTES + 1)
        .await
        .map_err(|_| ApiError::invalid_request())?;
    if bytes.is_empty() || bytes.len() > MAXIMUM_COMMAND_BODY_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "ENGAGEMENT_BODY_BOUND_EXCEEDED",
            "The engagement command exceeds its request-body bound.",
            false,
        ));
    }
    let value = parse_unique_json(&bytes).map_err(|_| ApiError::invalid_request())?;
    serde_json::from_value(value).map_err(|_| ApiError::invalid_request())
}

async fn run_domain<T, F>(operation: F) -> Response
where
    T: Serialize + Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(operation).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => ApiError::from_kernel(&error).into_response(),
        Err(_) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SETUP_TASK_FAILED",
            "The bounded local engagement service is unavailable.",
            false,
        )
        .into_response(),
    }
}

fn validate_command_headers(
    headers: &HeaderMap,
    intent: &str,
    content_type: &str,
) -> std::result::Result<String, ApiError> {
    if headers.contains_key("forwarded")
        || headers
            .keys()
            .any(|name| name.as_str().starts_with("x-forwarded-"))
    {
        return Err(ApiError::invalid_transport());
    }
    let host = loopback_host(headers)?;
    let origin = single_header(headers, ORIGIN.as_str())?;
    if origin != format!("http://{host}") {
        return Err(ApiError::invalid_transport());
    }
    if !single_header(headers, CONTENT_TYPE.as_str())?.eq_ignore_ascii_case(content_type) {
        return Err(ApiError::invalid_transport());
    }
    if single_header(headers, "x-migration-stream-intent")? != intent {
        return Err(ApiError::invalid_transport());
    }
    let key = single_header(headers, "idempotency-key")?;
    if !safe_idempotency_key(key) {
        return Err(ApiError::invalid_transport());
    }
    Ok(key.to_owned())
}

async fn enforce_transport_bounds(request: Request<Body>, next: Next) -> Response {
    let header_bytes = request
        .headers()
        .iter()
        .try_fold(0_usize, |total, (name, value)| {
            total
                .checked_add(name.as_str().len())?
                .checked_add(value.as_bytes().len())
        });
    if request.uri().to_string().len() > MAXIMUM_URI_BYTES
        || request.headers().len() > MAXIMUM_HEADER_COUNT
        || header_bytes.is_none_or(|bytes| bytes > MAXIMUM_HEADER_BYTES)
        || loopback_host(request.headers()).is_err()
    {
        return ApiError::invalid_request().into_response();
    }
    if matches!(request.method(), &Method::GET | &Method::HEAD)
        && request_has_body(request.headers())
    {
        return ApiError::invalid_request().into_response();
    }
    if matches!(request.method(), &Method::POST | &Method::PUT) && request.uri().query().is_some() {
        return ApiError::invalid_request().into_response();
    }
    next.run(request).await
}

fn loopback_host(headers: &HeaderMap) -> std::result::Result<&str, ApiError> {
    let mut values = headers.get_all(HOST).iter();
    let value = values
        .next()
        .filter(|_| values.next().is_none())
        .ok_or_else(ApiError::invalid_transport)?;
    let raw = value.to_str().map_err(|_| ApiError::invalid_transport())?;
    let authority = raw
        .parse::<axum::http::uri::Authority>()
        .map_err(|_| ApiError::invalid_transport())?;
    if raw.contains('@') || !valid_authority_port(&authority) || !host_is_loopback(authority.host())
    {
        return Err(ApiError::invalid_transport());
    }
    Ok(raw)
}

fn valid_authority_port(authority: &axum::http::uri::Authority) -> bool {
    let suffix = &authority.as_str()[authority.host().len()..];
    suffix.is_empty()
        || suffix
            .strip_prefix(':')
            .is_some_and(|port| !port.is_empty() && port.parse::<u16>().is_ok())
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

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> std::result::Result<&'a str, ApiError> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .filter(|_| values.next().is_none())
        .ok_or_else(ApiError::invalid_transport)?;
    value.to_str().map_err(|_| ApiError::invalid_transport())
}

fn content_length(headers: &HeaderMap) -> Option<std::result::Result<u64, ()>> {
    headers.get(CONTENT_LENGTH).map(|value| {
        value
            .to_str()
            .map_err(|_| ())
            .and_then(|value| value.parse::<u64>().map_err(|_| ()))
    })
}

fn request_has_body(headers: &HeaderMap) -> bool {
    headers.contains_key("transfer-encoding")
        || headers
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value != "0")
}

fn safe_idempotency_key(value: &str) -> bool {
    value.len() >= MINIMUM_IDEMPOTENCY_KEY_BYTES
        && value.len() <= MAXIMUM_IDEMPOTENCY_KEY_BYTES
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assisted_runtime_tokens_have_one_platform_specific_lowercase_hex_shape() {
        let unix = "0123456789abcdef".repeat(2);
        let windows = "0123456789abcdef".repeat(4);
        assert!(lowercase_hex_token_has_exact_length(&unix, 32));
        assert!(lowercase_hex_token_has_exact_length(&windows, 64));
        for invalid in [
            "0".repeat(31),
            "0".repeat(33),
            "0".repeat(63),
            "0".repeat(65),
            "A0".repeat(16),
            "A0".repeat(32),
            "g0".repeat(16),
            format!("{} ", "00".repeat(16)),
        ] {
            assert!(!lowercase_hex_token_has_exact_length(&invalid, 32));
            assert!(!lowercase_hex_token_has_exact_length(&invalid, 64));
        }
        #[cfg(unix)]
        {
            assert!(assisted_runtime_session_token_is_valid(&unix));
            assert!(!assisted_runtime_session_token_is_valid(&windows));
        }
        #[cfg(windows)]
        {
            assert!(!assisted_runtime_session_token_is_valid(&unix));
            assert!(assisted_runtime_session_token_is_valid(&windows));
        }
    }

    #[tokio::test]
    async fn command_decoder_rejects_duplicate_object_keys() {
        let error = decode_command::<serde_json::Value>(Body::from(
            br#"{"schemaVersion":"test/v1","parameters":{"tableId":"one","tableId":"two"}}"#
                .as_slice(),
        ))
        .await
        .expect_err("duplicate command keys must fail before typed decoding");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "INVALID_ENGAGEMENT_REQUEST");
    }

    #[tokio::test]
    async fn command_decoder_accepts_unique_bounded_json() {
        let value = decode_command::<serde_json::Value>(Body::from(
            br#"{"schemaVersion":"test/v1","parameters":{"tableId":"one"}}"#.as_slice(),
        ))
        .await
        .expect("unique bounded command JSON should decode");

        assert_eq!(value["parameters"]["tableId"], "one");
    }

    #[test]
    fn assisted_runtime_authority_heads_exclude_operator_text_and_paths() {
        let marker = "RECEIVED-VALUE-MUST-NOT-ENTER-RUNTIME-IDENTITY";
        let candidate = narrow_runtime_candidate_head(
            &crate::reconciliation_read_model::ResultCandidateSummary {
                candidate_id: "candidate-1".to_owned(),
                snapshot_id: "snapshot-1".to_owned(),
                snapshot_hash: "a".repeat(64),
                candidate_hash: "b".repeat(64),
                current: true,
                eligible: true,
                included_count: 3,
                excluded_count: 1,
                blocked_count: 0,
                warning_codes: vec![],
                required_acknowledgments: vec![],
                creator: marker.to_owned(),
                preparation_reason: marker.to_owned(),
                created_at: "2026-01-01T00:00:00Z".to_owned(),
            },
        );
        let action =
            narrow_runtime_action_head(&crate::reconciliation_read_model::ResultActionSummary {
                action_id: "action-1".to_owned(),
                action_kind: "APPROVED".to_owned(),
                action_hash: "c".repeat(64),
                candidate_id: "candidate-1".to_owned(),
                candidate_hash: "b".repeat(64),
                actor: marker.to_owned(),
                reason: marker.to_owned(),
                generation: 1,
                predecessor_action_id: None,
                occurred_at: "2026-01-01T00:00:00Z".to_owned(),
            });
        let publication = narrow_runtime_publication_head(
            crate::publication_read_model::PublicationAuthorityHead {
                total_count: 1,
                latest: Some(crate::publication_read_model::PublicationSummaryItem {
                    publication_id: "publication-1".to_owned(),
                    job_id: "job-1".to_owned(),
                    candidate_id: "candidate-1".to_owned(),
                    approval_id: "action-1".to_owned(),
                    status: "COMPLETE".to_owned(),
                    practitioner: marker.to_owned(),
                    publication_reason: marker.to_owned(),
                    semantic_publication_hash: Some("d".repeat(64)),
                    physical_archive_digest: Some("e".repeat(64)),
                    package_size_bytes: Some(1_024),
                    archive_relative_path: Some(marker.to_owned()),
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                    completed_at: Some("2026-01-01T00:00:00Z".to_owned()),
                }),
            },
        );
        let preimage = serde_json::json!({
            "candidate": candidate,
            "action": action,
            "publication": publication,
        })
        .to_string();
        assert!(!preimage.contains(marker));
        for omitted in [
            "creator",
            "preparationReason",
            "actor",
            "reason",
            "practitioner",
            "publicationReason",
            "archiveRelativePath",
        ] {
            assert!(!preimage.contains(omitted), "{omitted} entered preimage");
        }
        assert!(preimage.contains(&"b".repeat(64)));
        assert!(preimage.contains(&"d".repeat(64)));
    }
}

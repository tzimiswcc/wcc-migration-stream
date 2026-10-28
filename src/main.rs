use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use migration_stream::{
    KernelError, Result,
    benchmark::run_benchmark_case,
    canonical::canonical_hash,
    edmx::{EdmxLimits, EdmxSourceContext},
    fixture::{FixtureSpec, generate_fixture},
    intake::{IngestControl, ingest_package_with_control, inspect_archive},
    mapping::{inspect_mapping, validate_mapping},
    metadata::import_edmx_to_workspace,
    object_workspace::{
        EstablishObjectWorkspaceCommand, INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA,
        ObjectWorkspaceAction, ObjectWorkspaceState, adopt_for_local_automation,
        establish_for_local_automation,
    },
    policy::IntakePolicy,
    profile::{require_approved, validate_profile},
    profile_registry::{
        RegistryLimits, approve_profile, deprecate_profile, inspect_profile, register_profile,
        retire_profile,
    },
    profiling::{ProfilingControl, ProfilingPolicy, run_profile_with_control},
    project_object::{OpenProject, bind_project, create_project, open_project},
    read_model::{CleanupFilters, IssueFilters, IssueSeverity, PageRequest, ReadService},
    runtime_bundle::compiled_bundle_build_identity,
    state::{integrity_check, recover_workspace, workspace_status},
    validation::{
        ValidationControl, ValidationPolicy, run_validation_with_control, validation_status,
    },
};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "migration-stream",
    version,
    about = "Migration-readiness ingestion and bounded profiling kernel"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Internal build-provenance handshake used by the assisted bundle assembler.
    #[command(name = "__bundle-build-identity", hide = true)]
    BundleBuildIdentity,
    /// Create, inspect or explicitly bind a trusted local project object.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Establish the explicit identity of one startup-selected local workspace.
    ObjectWorkspace {
        #[command(subcommand)]
        command: ObjectWorkspaceCommand,
    },
    /// Spool, inspect, ingest, and reconcile a synthetic ZIP/CSV package.
    Ingest {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        policy: Option<PathBuf>,
        /// Cooperative cancellation token polled at safe boundaries.
        #[arg(long)]
        cancel_file: Option<PathBuf>,
        /// Deterministic test/benchmark cancellation after committed batches.
        #[arg(long, hide = true)]
        cancel_after_batches: Option<u64>,
    },
    /// Inspect a ZIP without extracting or ingesting it.
    Inspect {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        policy: Option<PathBuf>,
    },
    /// Show durable workspace state without changing it.
    Status {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Classify abandoned work as interrupted and reconcile owned partial artifacts.
    Recover {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Verify `SQLite`, reconciliation, and immutable package hashes.
    Integrity {
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Validate profile syntax/semantics or require approved authority.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Import immutable technical metadata as `GENERATED_DRAFT` evidence.
    Metadata {
        #[command(subcommand)]
        command: MetadataCommand,
    },
    /// Generate deterministic synthetic ZIP/CSV fixtures.
    Fixture {
        #[command(subcommand)]
        command: FixtureCommand,
    },
    /// Run one guarded benchmark-matrix case and validate its evidence.
    Benchmark {
        #[command(subcommand)]
        command: BenchmarkCommand,
    },
    /// Validate and inspect immutable source-to-canonical mappings.
    Mapping {
        #[command(subcommand)]
        command: MappingCommand,
    },
    /// Execute and inspect bounded canonical validation.
    Validation {
        #[command(subcommand)]
        command: ValidationCommand,
    },
    /// Query versioned bounded practitioner read contracts.
    Read {
        #[command(subcommand)]
        command: ReadCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// Create one new object directory; default leaves explicit initialization pending.
    Create {
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        display_name: String,
        /// Explicit new-empty-object intent: initialize its workspace and bind it.
        /// This never resumes or adopts an existing directory.
        #[arg(long)]
        initialize_and_bind: bool,
        /// Existing workspace identity metadata, supplied by the trusted caller.
        #[arg(
            long,
            requires = "initialize_and_bind",
            required_if_eq("initialize_and_bind", "true")
        )]
        migration_object_name_or_scope: Option<String>,
        /// Existing workspace attribution metadata; no approval authority is granted.
        #[arg(
            long,
            requires = "initialize_and_bind",
            required_if_eq("initialize_and_bind", "true")
        )]
        creator_attribution: Option<String>,
    },
    /// Validate and report a pending or bound descriptor without mutation.
    Inspect {
        #[arg(long)]
        project: PathBuf,
    },
    /// Explicitly resume only the exact retained interrupted initialization.
    /// Leaves project binding as a separate operation.
    ResumeInitialization {
        #[arg(long)]
        project: PathBuf,
    },
    /// Explicitly bind a pending receipt to its already active workspace.
    Bind {
        #[arg(long)]
        project: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum ObjectWorkspaceCommand {
    /// Initialize one exact empty synthetic/local-automation workspace.
    Initialize {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        display_name: String,
        #[arg(long)]
        migration_object_name_or_scope: String,
        #[arg(long)]
        creator_attribution: String,
    },
    /// Explicitly adopt one exact existing synthetic/local-automation workspace.
    Adopt {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        display_name: String,
        #[arg(long)]
        migration_object_name_or_scope: String,
        #[arg(long)]
        creator_attribution: String,
    },
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    /// Profile one completed intake job using bounded textual state.
    Run {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        job: String,
        #[arg(long)]
        policy: PathBuf,
        /// Cooperative cancellation token polled at safe boundaries.
        #[arg(long)]
        cancel_file: Option<PathBuf>,
        /// Deterministic test/benchmark cancellation after processed batches.
        #[arg(long, hide = true)]
        cancel_after_batches: Option<u64>,
    },
    /// Validate a profile; validation never grants approval.
    Validate {
        #[arg(long)]
        schema: PathBuf,
        #[arg(long)]
        profile: PathBuf,
    },
    /// Require an independently approved profile with complete evidence.
    RequireApproved {
        #[arg(long)]
        schema: PathBuf,
        #[arg(long)]
        profile: PathBuf,
    },
    /// Register one immutable validated draft; registration grants no authority.
    Register {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        schema: PathBuf,
        #[arg(long)]
        profile: PathBuf,
    },
    /// Inspect immutable profile bytes, lifecycle, and local authority separately.
    Inspect {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        profile_id: String,
        #[arg(long)]
        profile_version: String,
    },
    /// Explicitly approve one exact reviewed profile for local mapping/validation only.
    Approve {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        profile_id: String,
        #[arg(long)]
        profile_version: String,
        #[arg(long)]
        profile_hash: String,
        #[arg(long)]
        approver: String,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        acknowledge_local_mapping_validation_authority: bool,
    },
    /// Block new runs while retaining immutable approved profile history.
    Deprecate {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        profile_id: String,
        #[arg(long)]
        profile_version: String,
        #[arg(long)]
        profile_hash: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        reason: String,
    },
    /// Retire a profile terminally while retaining its immutable history.
    Retire {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        profile_id: String,
        #[arg(long)]
        profile_version: String,
        #[arg(long)]
        profile_hash: String,
        #[arg(long)]
        actor: String,
        #[arg(long)]
        reason: String,
    },
}

#[derive(Debug, Subcommand)]
enum MappingCommand {
    /// Validate and immutably register a mapping against accepted source evidence.
    Validate {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        job: String,
        #[arg(long)]
        schema: PathBuf,
        #[arg(long)]
        profile_schema: PathBuf,
        #[arg(long)]
        mapping: PathBuf,
    },
    /// Inspect one immutable mapping version.
    Inspect {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        mapping_id: String,
        #[arg(long)]
        mapping_version: String,
    },
}

#[derive(Debug, Subcommand)]
enum ValidationCommand {
    /// Run bounded validation over accepted persisted source rows.
    Run {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        job: String,
        #[arg(long)]
        mapping_id: String,
        #[arg(long)]
        mapping_version: String,
        #[arg(long)]
        profile_schema: PathBuf,
        #[arg(long)]
        policy: PathBuf,
        #[arg(long)]
        cancel_file: Option<PathBuf>,
        #[arg(long, hide = true)]
        cancel_after_batches: Option<u64>,
    },
    /// Inspect any validation attempt lifecycle state.
    Status {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        run: String,
    },
    /// List validation issues through bounded allow-listed filters.
    ListIssues {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        run: String,
        #[arg(long, default_value_t = 50)]
        page_size: u32,
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long)]
        severity: Option<String>,
        #[arg(long)]
        rule_id: Option<String>,
        #[arg(long)]
        entity_id: Option<String>,
        #[arg(long)]
        field_id: Option<String>,
        #[arg(long)]
        source_file: Option<String>,
        #[arg(long)]
        source_ordinal: Option<u64>,
    },
}

#[derive(Debug, Subcommand)]
enum ReadCommand {
    JobSummary {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        job: String,
    },
    FileManifest {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        job: String,
        #[command(flatten)]
        page: CliPage,
    },
    ProfilingSummary {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        run: String,
        #[arg(long)]
        include_samples: bool,
        #[command(flatten)]
        page: CliPage,
    },
    ProfileSummary {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        profile_id: String,
        #[arg(long)]
        profile_version: String,
        #[arg(long)]
        profile_hash: String,
    },
    MappingCoverage {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        mapping_id: String,
        #[arg(long)]
        mapping_version: String,
        #[arg(long)]
        mapping_hash: String,
        #[command(flatten)]
        page: CliPage,
    },
    ValidationSummary {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        run: String,
    },
    RowDetail {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        run: String,
        #[arg(long)]
        source_file: String,
        #[arg(long)]
        source_ordinal: u64,
    },
    ReconciliationSummary {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        job: String,
        #[arg(long)]
        validation_run: Option<String>,
        #[command(flatten)]
        page: CliPage,
    },
    OperationalCleanupSummary {
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        namespace: Option<String>,
        #[arg(long)]
        state: Option<String>,
        #[command(flatten)]
        page: CliPage,
    },
}

#[derive(Clone, Debug, clap::Args)]
struct CliPage {
    #[arg(long, default_value_t = 50)]
    page_size: u32,
    #[arg(long)]
    cursor: Option<String>,
}

impl From<CliPage> for PageRequest {
    fn from(value: CliPage) -> Self {
        Self {
            page_size: value.page_size,
            cursor: value.cursor,
        }
    }
}

#[derive(Debug, Subcommand)]
enum FixtureCommand {
    /// Generate a fixture and stable sidecar manifest with create-new semantics.
    Generate {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        output_directory: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum BenchmarkCommand {
    /// Execute one isolated matrix case under its resource guards.
    Run {
        #[arg(long)]
        matrix: PathBuf,
        #[arg(long)]
        case: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        host_label: String,
    },
}

#[derive(Debug, Subcommand)]
enum MetadataCommand {
    /// Preserve and parse a sanitized EDMX snapshot without network access.
    ImportEdmx {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        workspace: PathBuf,
        #[arg(long)]
        system: String,
        #[arg(long)]
        client: String,
        #[arg(long)]
        release: String,
        #[arg(long)]
        artifact: Option<String>,
    },
}

#[derive(Debug, Serialize)]
struct Success<T> {
    schema_version: &'static str,
    ok: bool,
    result: T,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectFailureContext {
    phase: &'static str,
    directory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    descriptor_path: Option<String>,
    paths_are_lossy: bool,
    next_action: &'static str,
}

#[derive(Debug)]
struct CommandFailure {
    cause: KernelError,
    project: Option<Box<ProjectFailureContext>>,
}

impl From<KernelError> for CommandFailure {
    fn from(cause: KernelError) -> Self {
        Self {
            cause,
            project: None,
        }
    }
}

impl From<serde_json::Error> for CommandFailure {
    fn from(error: serde_json::Error) -> Self {
        KernelError::from(error).into()
    }
}

type CommandResult<T> = std::result::Result<T, CommandFailure>;

fn project_failure(
    cause: KernelError,
    phase: &'static str,
    directory: &std::path::Path,
    descriptor_path: Option<PathBuf>,
) -> CommandFailure {
    let next_action = match phase {
        "CREATE" => {
            "Inspect the selected directory explicitly; create never overwrites or resumes existing material."
        }
        "INITIALIZE" => {
            "Inspect the retained pending descriptor and use the existing explicit workspace initialization/recovery workflow, then project bind. Retrying create does not resume it."
        }
        "RESUME_INITIALIZATION" => {
            "Inspect the retained project and lifecycle. Resume only its exact pending initialization explicitly after resolving the reported failure; bind separately when active."
        }
        _ => {
            "Inspect the retained pending or final descriptor. Binding requires an active workspace; use project bind explicitly if final publication is still pending."
        }
    };
    let paths_are_lossy = directory.to_str().is_none()
        || descriptor_path
            .as_ref()
            .is_some_and(|path| path.to_str().is_none());
    CommandFailure {
        cause,
        project: Some(Box::new(ProjectFailureContext {
            phase,
            directory: directory.to_string_lossy().into_owned(),
            descriptor_path: descriptor_path.map(|path| path.to_string_lossy().into_owned()),
            paths_are_lossy,
            next_action,
        })),
    }
}

fn project_view(project: &OpenProject) -> Result<serde_json::Value> {
    let required_action = if project.descriptor.binding.is_some() {
        None
    } else if project.lifecycle.state == ObjectWorkspaceState::Active {
        Some("BIND_PROJECT")
    } else if project
        .lifecycle
        .pending_command
        .as_ref()
        .is_some_and(|command| command.schema_version == INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA)
    {
        Some("RESUME_EXACT_WORKSPACE_INITIALIZATION")
    } else {
        Some("INITIALIZE_WORKSPACE_EXPLICITLY")
    };
    let paths = serde_json::to_value([
        project.descriptor_path.as_path(),
        project.workspace.root(),
        project.input.as_path(),
        project.output.as_path(),
    ])?;
    Ok(serde_json::json!({
        "schemaVersion": "migration-stream/project-cli/v1",
        "descriptorPath": paths[0], "descriptor": project.descriptor,
        "workspacePath": paths[1], "lifecycle": project.lifecycle,
        "inputPath": paths[2], "outputPath": paths[3], "requiredAction": required_action,
    }))
}

fn project_response(
    project: &OpenProject,
    phase: &'static str,
) -> CommandResult<serde_json::Value> {
    project_view(project).map_err(|cause| {
        project_failure(
            cause,
            phase,
            project
                .descriptor_path
                .parent()
                .expect("opened project parent"),
            Some(project.descriptor_path.clone()),
        )
    })
}

fn resume_project_initialization(project: &std::path::Path) -> CommandResult<serde_json::Value> {
    let directory = project.parent().unwrap_or(std::path::Path::new("."));
    let failure = |cause| {
        project_failure(
            cause,
            "RESUME_INITIALIZATION",
            directory,
            Some(project.to_path_buf()),
        )
    };
    let pending = open_project(project).map_err(failure)?;
    let command = pending
        .lifecycle
        .pending_command
        .as_ref()
        .filter(|command| {
            pending.descriptor.binding.is_none()
                && pending.lifecycle.state == ObjectWorkspaceState::InitializeRequired
                && command.schema_version == INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA
        })
        .ok_or_else(|| {
            failure(KernelError::Conflict(
                "project has no exact interrupted initialization to resume; inspect its lifecycle"
                    .to_owned(),
            ))
        })?;
    // The service retains the original command/CAS/key and owns the writer lock.
    // Never rebuild the command from current state or silently bind the project.
    pending.workspace.initialize(command).map_err(failure)?;
    let active = open_project(project).map_err(failure)?;
    project_response(&active, "RESUME_INITIALIZATION")
}

fn execute_project(command: ProjectCommand) -> CommandResult<serde_json::Value> {
    match command {
        ProjectCommand::Create {
            directory,
            display_name,
            initialize_and_bind,
            migration_object_name_or_scope,
            creator_attribution,
        } => {
            let initialization = if initialize_and_bind {
                Some((
                    migration_object_name_or_scope.ok_or_else(|| {
                        KernelError::PolicyInvalid(
                            "initialize-and-bind requires migration object name or scope"
                                .to_owned(),
                        )
                    })?,
                    creator_attribution.ok_or_else(|| {
                        KernelError::PolicyInvalid(
                            "initialize-and-bind requires creator attribution".to_owned(),
                        )
                    })?,
                ))
            } else {
                None
            };
            let pending = create_project(&directory, &display_name)
                .map_err(|cause| project_failure(cause, "CREATE", &directory, None))?;
            let Some((scope, creator)) = initialization else {
                return project_response(&pending, "CREATE");
            };
            let descriptor = pending.descriptor_path.clone();
            let directory = descriptor
                .parent()
                .expect("opened project parent")
                .to_path_buf();
            if pending.lifecycle.state != ObjectWorkspaceState::InitializeRequired
                || pending.lifecycle.pending_command.is_some()
            {
                return Err(project_failure(KernelError::Conflict("new object is no longer a fresh empty workspace; inspect its lifecycle explicitly".to_owned()), "INITIALIZE", &directory, Some(descriptor)));
            }
            // Reuse the existing initialization command/semantic key, while
            // retaining this freshly created workspace and its exact CAS.
            let semantic = canonical_hash(&serde_json::json!({
                "action": ObjectWorkspaceAction::Initialize,
                "displayName": display_name,
                "migrationObjectNameOrScope": scope,
                "creatorAttribution": creator,
            }))?;
            pending
                .workspace
                .initialize(&EstablishObjectWorkspaceCommand {
                    schema_version: INITIALIZE_OBJECT_WORKSPACE_COMMAND_SCHEMA.to_owned(),
                    expected_state_hash: pending.lifecycle.state_hash.clone(),
                    display_name,
                    migration_object_name_or_scope: scope,
                    creator_attribution: creator,
                    idempotency_key: format!("local-automation-{semantic}"),
                })
                .map_err(|cause| {
                    project_failure(cause, "INITIALIZE", &directory, Some(descriptor.clone()))
                })?;
            let bound = bind_project(&descriptor)
                .map_err(|cause| project_failure(cause, "BIND", &directory, Some(descriptor)))?;
            project_response(&bound, "BIND")
        }
        ProjectCommand::Inspect { project } => Ok(project_view(&open_project(project)?)?),
        ProjectCommand::ResumeInitialization { project } => resume_project_initialization(&project),
        ProjectCommand::Bind { project } => {
            let directory = project
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf();
            let bound = bind_project(&project)
                .map_err(|cause| project_failure(cause, "BIND", &directory, Some(project)))?;
            project_response(&bound, "BIND")
        }
    }
}

#[derive(Debug, Serialize)]
struct Failure<'a> {
    schema_version: &'static str,
    ok: bool,
    code: &'a str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<&'a ProjectFailureContext>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match execute(cli.command) {
        Ok(result) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&Success {
                    schema_version: "migration-stream/cli/v1",
                    ok: true,
                    result,
                })
                .expect("serializing a JSON value cannot fail")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            let failure = Failure {
                schema_version: "migration-stream/cli/v1",
                ok: false,
                code: error.cause.code(),
                message: error.cause.to_string(),
                project: error.project.as_deref(),
            };
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&failure)
                    .expect("serializing a JSON error cannot fail")
            );
            ExitCode::from(1)
        }
    }
}

#[allow(clippy::too_many_lines)]
fn execute(command: Command) -> CommandResult<serde_json::Value> {
    match command {
        Command::Project { command } => execute_project(command),
        Command::BundleBuildIdentity => {
            Ok(serde_json::to_value(compiled_bundle_build_identity()?)?)
        }
        Command::ObjectWorkspace { command } => match command {
            ObjectWorkspaceCommand::Initialize {
                workspace,
                display_name,
                migration_object_name_or_scope,
                creator_attribution,
            } => {
                std::fs::create_dir_all(&workspace)
                    .map_err(|error| KernelError::io(&workspace, error))?;
                Ok(serde_json::to_value(establish_for_local_automation(
                    &workspace,
                    &display_name,
                    &migration_object_name_or_scope,
                    &creator_attribution,
                )?)?)
            }
            ObjectWorkspaceCommand::Adopt {
                workspace,
                display_name,
                migration_object_name_or_scope,
                creator_attribution,
            } => Ok(serde_json::to_value(adopt_for_local_automation(
                &workspace,
                &display_name,
                &migration_object_name_or_scope,
                &creator_attribution,
            )?)?),
        },
        Command::Ingest {
            source,
            workspace,
            policy,
            cancel_file,
            cancel_after_batches,
        } => {
            let policy = IntakePolicy::from_path(policy.as_deref())?;
            let control = IngestControl {
                cancel_file,
                cancel_after_batches,
            };
            Ok(serde_json::to_value(ingest_package_with_control(
                &source, &workspace, &policy, &control,
            )?)?)
        }
        Command::Inspect { source, policy } => {
            let policy = IntakePolicy::from_path(policy.as_deref())?;
            Ok(serde_json::to_value(inspect_archive(&source, &policy)?)?)
        }
        Command::Status { workspace } => Ok(serde_json::to_value(workspace_status(&workspace)?)?),
        Command::Recover { workspace } => Ok(serde_json::to_value(recover_workspace(&workspace)?)?),
        Command::Integrity { workspace } => {
            let report = integrity_check(&workspace)?;
            if !report.ok {
                return Err(KernelError::IntegrityFailed(report.issues.join("; ")).into());
            }
            Ok(serde_json::to_value(report)?)
        }
        Command::Profile { command } => match command {
            ProfileCommand::Run {
                workspace,
                job,
                policy,
                cancel_file,
                cancel_after_batches,
            } => {
                let policy = ProfilingPolicy::from_path(&policy)?;
                let control = ProfilingControl {
                    cancel_file,
                    cancel_after_batches,
                };
                Ok(serde_json::to_value(run_profile_with_control(
                    &workspace, &job, &policy, &control,
                )?)?)
            }
            ProfileCommand::Validate { schema, profile } => {
                let validated = validate_profile(&schema, &profile)?;
                Ok(serde_json::to_value(validated.summary())?)
            }
            ProfileCommand::RequireApproved { schema, profile } => {
                let validated = validate_profile(&schema, &profile)?;
                let approved = require_approved(validated)?;
                Ok(serde_json::to_value(approved.summary())?)
            }
            ProfileCommand::Register {
                workspace,
                schema,
                profile,
            } => Ok(serde_json::to_value(register_profile(
                &workspace,
                &schema,
                &profile,
                &RegistryLimits::default(),
            )?)?),
            ProfileCommand::Inspect {
                workspace,
                profile_id,
                profile_version,
            } => Ok(serde_json::to_value(inspect_profile(
                &workspace,
                &profile_id,
                &profile_version,
            )?)?),
            ProfileCommand::Approve {
                workspace,
                profile_id,
                profile_version,
                profile_hash,
                approver,
                reason,
                acknowledge_local_mapping_validation_authority,
            } => Ok(serde_json::to_value(approve_profile(
                &workspace,
                &profile_id,
                &profile_version,
                &profile_hash,
                &approver,
                &reason,
                acknowledge_local_mapping_validation_authority,
                &RegistryLimits::default(),
            )?)?),
            ProfileCommand::Deprecate {
                workspace,
                profile_id,
                profile_version,
                profile_hash,
                actor,
                reason,
            } => Ok(serde_json::to_value(deprecate_profile(
                &workspace,
                &profile_id,
                &profile_version,
                &profile_hash,
                &actor,
                &reason,
                &RegistryLimits::default(),
            )?)?),
            ProfileCommand::Retire {
                workspace,
                profile_id,
                profile_version,
                profile_hash,
                actor,
                reason,
            } => Ok(serde_json::to_value(retire_profile(
                &workspace,
                &profile_id,
                &profile_version,
                &profile_hash,
                &actor,
                &reason,
                &RegistryLimits::default(),
            )?)?),
        },
        Command::Metadata { command } => match command {
            MetadataCommand::ImportEdmx {
                source,
                workspace,
                system,
                client,
                release,
                artifact,
            } => {
                let artifact = artifact.unwrap_or_else(|| {
                    source
                        .file_name()
                        .map_or_else(|| source.as_os_str(), |name| name)
                        .to_string_lossy()
                        .into_owned()
                });
                let context = EdmxSourceContext {
                    artifact,
                    system: Some(system),
                    client: Some(client),
                    release: Some(release),
                };
                Ok(serde_json::to_value(import_edmx_to_workspace(
                    &source,
                    &workspace,
                    context,
                    &EdmxLimits::default(),
                )?)?)
            }
        },
        Command::Fixture { command } => match command {
            FixtureCommand::Generate {
                spec,
                output_directory,
            } => {
                let spec = FixtureSpec::from_path(&spec)?;
                Ok(serde_json::to_value(generate_fixture(
                    &spec,
                    &output_directory,
                )?)?)
            }
        },
        Command::Benchmark { command } => match command {
            BenchmarkCommand::Run {
                matrix,
                case,
                output,
                host_label,
            } => Ok(serde_json::to_value(run_benchmark_case(
                &matrix,
                &case,
                &output,
                &host_label,
            )?)?),
        },
        Command::Mapping { command } => match command {
            MappingCommand::Validate {
                workspace,
                job,
                schema,
                profile_schema,
                mapping,
            } => Ok(serde_json::to_value(validate_mapping(
                &workspace,
                &job,
                &schema,
                &profile_schema,
                &mapping,
            )?)?),
            MappingCommand::Inspect {
                workspace,
                mapping_id,
                mapping_version,
            } => Ok(serde_json::to_value(inspect_mapping(
                &workspace,
                &mapping_id,
                &mapping_version,
            )?)?),
        },
        Command::Validation { command } => match command {
            ValidationCommand::Run {
                workspace,
                job,
                mapping_id,
                mapping_version,
                profile_schema,
                policy,
                cancel_file,
                cancel_after_batches,
            } => {
                let policy = ValidationPolicy::from_path(&policy)?;
                let control = ValidationControl {
                    cancel_file,
                    cancel_after_batches,
                };
                Ok(serde_json::to_value(run_validation_with_control(
                    &workspace,
                    &job,
                    &mapping_id,
                    &mapping_version,
                    &profile_schema,
                    &policy,
                    &control,
                )?)?)
            }
            ValidationCommand::Status { workspace, run } => {
                Ok(serde_json::to_value(validation_status(&workspace, &run)?)?)
            }
            ValidationCommand::ListIssues {
                workspace,
                run,
                page_size,
                cursor,
                severity,
                rule_id,
                entity_id,
                field_id,
                source_file,
                source_ordinal,
            } => {
                let filters = IssueFilters {
                    severity: severity.as_deref().map(parse_issue_severity).transpose()?,
                    rule_id,
                    entity_id,
                    field_id,
                    source_file,
                    source_ordinal,
                };
                let page = PageRequest { page_size, cursor };
                Ok(serde_json::to_value(
                    ReadService::new(workspace).issue_list(&run, &filters, &page)?,
                )?)
            }
        },
        Command::Read { command } => execute_read(command).map_err(Into::into),
    }
}

#[allow(clippy::too_many_lines)]
fn execute_read(command: ReadCommand) -> Result<serde_json::Value> {
    match command {
        ReadCommand::JobSummary { workspace, job } => Ok(serde_json::to_value(
            ReadService::new(workspace).job_summary(&job)?,
        )?),
        ReadCommand::FileManifest {
            workspace,
            job,
            page,
        } => Ok(serde_json::to_value(
            ReadService::new(workspace).file_manifest(&job, &page.into())?,
        )?),
        ReadCommand::ProfilingSummary {
            workspace,
            run,
            include_samples,
            page,
        } => Ok(serde_json::to_value(
            ReadService::new(workspace).profiling_summary(&run, include_samples, &page.into())?,
        )?),
        ReadCommand::ProfileSummary {
            workspace,
            profile_id,
            profile_version,
            profile_hash,
        } => Ok(serde_json::to_value(
            ReadService::new(workspace).profile_view(
                &profile_id,
                &profile_version,
                &profile_hash,
            )?,
        )?),
        ReadCommand::MappingCoverage {
            workspace,
            mapping_id,
            mapping_version,
            mapping_hash,
            page,
        } => Ok(serde_json::to_value(
            ReadService::new(workspace).mapping_coverage(
                &mapping_id,
                &mapping_version,
                &mapping_hash,
                &page.into(),
            )?,
        )?),
        ReadCommand::ValidationSummary { workspace, run } => Ok(serde_json::to_value(
            ReadService::new(workspace).validation_summary(&run)?,
        )?),
        ReadCommand::RowDetail {
            workspace,
            run,
            source_file,
            source_ordinal,
        } => Ok(serde_json::to_value(
            ReadService::new(workspace).row_detail(&run, &source_file, source_ordinal)?,
        )?),
        ReadCommand::ReconciliationSummary {
            workspace,
            job,
            validation_run,
            page,
        } => Ok(serde_json::to_value(
            ReadService::new(workspace).reconciliation_summary(
                &job,
                validation_run.as_deref(),
                &page.into(),
            )?,
        )?),
        ReadCommand::OperationalCleanupSummary {
            workspace,
            namespace,
            state,
            page,
        } => {
            let filters = CleanupFilters { namespace, state };
            Ok(serde_json::to_value(
                ReadService::new(workspace).operational_cleanup(&filters, &page.into())?,
            )?)
        }
    }
}

fn parse_issue_severity(value: &str) -> Result<IssueSeverity> {
    match value.to_ascii_uppercase().as_str() {
        "ERROR" => Ok(IssueSeverity::Error),
        "WARNING" => Ok(IssueSeverity::Warning),
        "INFORMATION" => Ok(IssueSeverity::Information),
        _ => Err(KernelError::ReadModel(
            "severity must be ERROR, WARNING, or INFORMATION".to_owned(),
        )),
    }
}

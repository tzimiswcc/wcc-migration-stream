use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use migration_stream::{
    KernelError, Result,
    benchmark::run_benchmark_case,
    edmx::{EdmxLimits, EdmxSourceContext},
    fixture::{FixtureSpec, generate_fixture},
    intake::{IngestControl, ingest_package_with_control, inspect_archive},
    mapping::{inspect_mapping, validate_mapping},
    metadata::import_edmx_to_workspace,
    object_workspace::{adopt_for_local_automation, establish_for_local_automation},
    policy::IntakePolicy,
    profile::{require_approved, validate_profile},
    profile_registry::{
        RegistryLimits, approve_profile, deprecate_profile, inspect_profile, register_profile,
        retire_profile,
    },
    profiling::{ProfilingControl, ProfilingPolicy, run_profile_with_control},
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
struct Failure<'a> {
    schema_version: &'static str,
    ok: bool,
    code: &'a str,
    message: String,
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
                code: error.code(),
                message: error.to_string(),
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
fn execute(command: Command) -> Result<serde_json::Value> {
    match command {
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
                return Err(KernelError::IntegrityFailed(report.issues.join("; ")));
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
        Command::Read { command } => execute_read(command),
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

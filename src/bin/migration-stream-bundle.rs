use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, ValueEnum};
use migration_stream::runtime_bundle::{BundleAssemblyRequest, BundleSourceState, assemble_bundle};
use serde::Serialize;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SourceStateArgument {
    Clean,
    ExplicitUncommittedSnapshot,
}

impl From<SourceStateArgument> for BundleSourceState {
    fn from(value: SourceStateArgument) -> Self {
        match value {
            SourceStateArgument::Clean => Self::Clean,
            SourceStateArgument::ExplicitUncommittedSnapshot => Self::ExplicitUncommittedSnapshot,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "migration-stream-bundle",
    about = "Build-time assembler for one assisted local runtime bundle"
)]
struct Arguments {
    #[arg(long)]
    repository_root: PathBuf,
    /// One absolute, concrete Git executable verified by the build driver.
    /// The assembler never resolves Git from PATH.
    #[arg(long)]
    git_executable: PathBuf,
    #[arg(long)]
    frontend_assets_root: PathBuf,
    #[arg(long)]
    release_binaries_root: PathBuf,
    #[arg(long)]
    output_directory: PathBuf,
    #[arg(long)]
    product_commit: String,
    #[arg(long, value_enum)]
    source_state: SourceStateArgument,
    #[arg(long)]
    source_snapshot_sha256: String,
    #[arg(long)]
    frontend_assets_sha256: String,
    #[arg(long)]
    create_zip_archive: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Failure {
    schema_version: &'static str,
    ok: bool,
    code: &'static str,
    message: String,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let request = BundleAssemblyRequest {
        repository_root: arguments.repository_root,
        git_executable: arguments.git_executable,
        frontend_assets_root: arguments.frontend_assets_root,
        release_binaries_root: arguments.release_binaries_root,
        output_directory: arguments.output_directory,
        product_commit: arguments.product_commit,
        source_state: arguments.source_state.into(),
        source_snapshot_sha256: arguments.source_snapshot_sha256,
        frontend_assets_sha256: arguments.frontend_assets_sha256,
        create_zip_archive: arguments.create_zip_archive,
    };
    match assemble_bundle(&request) {
        Ok(result) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "schemaVersion": "migration-stream/assisted-local-runtime/assembly/v1",
                    "ok": true,
                    "result": result,
                }))
                .expect("serializing assembly result cannot fail")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::to_string_pretty(&Failure {
                    schema_version: "migration-stream/assisted-local-runtime/assembly/v1",
                    ok: false,
                    code: error.code(),
                    message: error.to_string(),
                })
                .expect("serializing assembly failure cannot fail")
            );
            ExitCode::FAILURE
        }
    }
}

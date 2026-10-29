//! Create-only build-time layout; never launches any packaged program.
use clap::Parser;
use migration_stream::runtime_bundle::{
    WindowsEngineeringLayoutRequest, assemble_windows_engineering_layout,
};
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(about = "Assemble trusted existing Windows engineering artifacts without executing them")]
struct Arguments {
    #[arg(long)]
    launcher: PathBuf,
    #[arg(long)]
    expected_launcher_sha256: String,
    #[arg(long)]
    runtime: PathBuf,
    #[arg(long)]
    output_directory: PathBuf,
    #[arg(long)]
    engineering_policy: String,
}
fn main() -> ExitCode {
    let Ok(a) = Arguments::try_parse() else {
        eprintln!(
            "Explicit engineering launcher, digest, runtime, output and policy arguments required"
        );
        return ExitCode::FAILURE;
    };
    let request = WindowsEngineeringLayoutRequest {
        launcher: a.launcher,
        expected_launcher_sha256: a.expected_launcher_sha256,
        runtime: a.runtime,
        output_directory: a.output_directory,
        engineering_policy: a.engineering_policy,
    };
    if let Ok(result) = assemble_windows_engineering_layout(&request) {
        println!("{}", serde_json::json!({"ok": true, "result": result}));
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "Windows engineering layout refused or failed; inspect retained private inputs and partial outputs"
        );
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inputs() -> Vec<&'static str> {
        vec![
            "assembler",
            "--launcher",
            "synthetic.exe",
            "--expected-launcher-sha256",
            "digest",
            "--runtime",
            "synthetic-runtime",
            "--output-directory",
            "synthetic-output",
            "--engineering-policy",
            "local-unsigned-engineering",
        ]
    }
    #[test]
    fn explicit_arguments_have_no_execution_or_verifier_override() {
        assert!(Arguments::try_parse_from(inputs()).is_ok());
        for flag in ["--skip-verify", "--target", "--port", "--run"] {
            let mut args = inputs();
            args.push(flag);
            assert!(Arguments::try_parse_from(args).is_err());
        }
    }
    #[test]
    fn every_input_is_required() {
        let args = inputs();
        for start in (1..args.len()).step_by(2) {
            let mut missing = args.clone();
            missing.drain(start..start + 2);
            assert!(Arguments::try_parse_from(missing).is_err());
        }
    }
}

//! Windows document-open coordination only; runtime authority remains in `open`.
//!
//! No entry point, process launch, browser launch or association is installed by
//! this module. The owner supplies its trusted intact runtime root and bounded
//! process/browser operations. Paths stay native UTF-16, never shell strings.

use serde::Deserialize;

pub const OUTPUT_LIMIT: usize = 65_536;
pub const CONTROLLER_TIMEOUT_SECONDS: u64 = 45;
const LOOPBACK: &str = "http://127.0.0.1:4173";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowsPath(Vec<u16>);

impl WindowsPath {
    /// Accept a drive-absolute or UNC native path without rewriting any units.
    /// Device/extended namespaces and normalization-dependent names are refused.
    pub fn from_units(units: Vec<u16>) -> Result<Self, Failure> {
        let drive = units.len() >= 4
            && matches!(units[0], 65..=90 | 97..=122)
            && units[1..3] == [u16::from(b':'), u16::from(b'\\')];
        let unc = units.starts_with(&[u16::from(b'\\'), u16::from(b'\\')]);
        if units.len() > 32_000 || (!drive && !unc) {
            return Err(Failure::InvalidPath);
        }
        let rest = &units[if drive { 3 } else { 2 }..];
        let components: Vec<_> = rest.split(|value| *value == u16::from(b'\\')).collect();
        if (unc && components.len() < 3)
            || components.iter().any(|part| {
                part.is_empty()
                    || *part == [u16::from(b'.')]
                    || *part == [u16::from(b'.'), u16::from(b'.')]
                    || matches!(part.last(), Some(32 | 46))
            })
            || rest.iter().any(|value| {
                *value < 32 || matches!(*value, 34 | 42 | 47 | 58 | 60 | 62 | 63 | 124 | 127)
            })
        {
            return Err(Failure::InvalidPath);
        }
        Ok(Self(units))
    }

    pub fn units(&self) -> &[u16] {
        &self.0
    }

    #[cfg(windows)]
    pub fn from_native(path: &std::ffi::OsStr) -> Result<Self, Failure> {
        use std::os::windows::ffi::OsStrExt;
        Self::from_units(path.encode_wide().collect())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    InvalidPath,
    InvalidDocument,
    DifferentDocument,
    Controller,
    InvalidResponse,
    Browser,
}

/// An operation failed; arbitrary process output or OS errors are not presented.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationFailed;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControllerInvocation {
    executable: WindowsPath,
    document: WindowsPath,
}

impl ControllerInvocation {
    pub fn executable(&self) -> &WindowsPath {
        &self.executable
    }

    /// Exactly three separate native arguments, never one quoted command line.
    pub fn arguments(&self) -> [Vec<u16>; 3] {
        [
            "open".encode_utf16().collect(),
            "--".encode_utf16().collect(),
            self.document.0.clone(),
        ]
    }

    /// Native construction only. The injected runner owns bounded capture,
    /// deadline and cleanup of this directly created controller child.
    #[cfg(windows)]
    pub fn command(&self) -> std::process::Command {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt, process::Stdio};
        let mut command = std::process::Command::new(OsString::from_wide(self.executable.units()));
        for argument in self.arguments() {
            command.arg(OsString::from_wide(&argument));
        }
        command
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
}

pub struct ControllerOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Trusted integration boundary, not an unbounded `Command::output` adapter.
/// Enforce the supplied limits while draining both pipes, use no inherited
/// environment or stdin, and bound cleanup to the directly owned child.
pub trait ControllerRunner {
    fn run(
        &mut self,
        invocation: &ControllerInvocation,
        timeout_seconds: u64,
        per_stream_limit: usize,
    ) -> Result<ControllerOutput, OperationFailed>;
}

/// This value cannot carry a controller-supplied URL or an arbitrary file path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BrowserDestination(&'static str);

impl BrowserDestination {
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

pub trait Browser {
    fn open(&mut self, destination: BrowserDestination) -> Result<(), OperationFailed>;
}

pub struct DocumentOpenCoordinator<R, B> {
    executable: WindowsPath,
    runner: R,
    browser: B,
    selected: Option<WindowsPath>,
}

impl<R: ControllerRunner, B: Browser> DocumentOpenCoordinator<R, B> {
    pub fn new(runtime_root: WindowsPath, runner: R, browser: B) -> Result<Self, Failure> {
        let mut executable = runtime_root.0;
        executable.extend("\\libexec\\migration-stream-local-runtime.exe".encode_utf16());
        Ok(Self {
            executable: WindowsPath::from_units(executable)?,
            runner,
            browser,
            selected: None,
        })
    }

    /// Exclusive mutable access serializes calls. One successful document stays
    /// pinned even if browser presentation fails; there is no object switching.
    pub fn open_documents(&mut self, documents: &[WindowsPath]) -> Result<(), Failure> {
        let [document] = documents else {
            return Err(Failure::InvalidDocument);
        };
        if self.selected.as_ref().is_some_and(|path| path != document) {
            return Err(Failure::DifferentDocument);
        }
        let invocation = ControllerInvocation {
            executable: self.executable.clone(),
            document: document.clone(),
        };
        let output = self
            .runner
            .run(&invocation, CONTROLLER_TIMEOUT_SECONDS, OUTPUT_LIMIT)
            .map_err(|_| Failure::Controller)?;
        if output.exit_code != 0 {
            return Err(Failure::Controller);
        }
        validate_output(&output)?;
        self.selected = Some(document.clone());
        self.browser
            .open(BrowserDestination(LOOPBACK))
            .map_err(|_| Failure::Browser)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    schema_version: String,
    ok: bool,
    result: Status,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    schema_version: String,
    classification: String,
    listen_address: String,
    url: String,
}

fn validate_output(output: &ControllerOutput) -> Result<(), Failure> {
    if output.stdout.len() > OUTPUT_LIMIT || output.stderr.len() > OUTPUT_LIMIT {
        return Err(Failure::InvalidResponse);
    }
    let envelope: Envelope =
        serde_json::from_slice(&output.stdout).map_err(|_| Failure::InvalidResponse)?;
    if envelope.schema_version != "migration-stream/assisted-local-runtime/cli/v1"
        || !envelope.ok
        || envelope.result.schema_version != "migration-stream/assisted-local-runtime/status/v2"
        || envelope.result.classification != "OWNED"
        || envelope.result.listen_address != "127.0.0.1:4173"
        || envelope.result.url != LOOPBACK
    {
        return Err(Failure::InvalidResponse);
    }
    Ok(())
}

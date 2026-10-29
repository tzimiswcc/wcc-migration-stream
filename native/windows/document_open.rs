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

// No background readers are created: inherited pipes cannot strand threads.
#[cfg(any(windows, test))]
pub(crate) mod process_owner {
    use super::{ControllerInvocation, ControllerOutput, OperationFailed};
    use std::time::Duration;

    const DEADLINE: Duration = Duration::from_secs(super::CONTROLLER_TIMEOUT_SECONDS);
    const CLEANUP: Duration = Duration::from_secs(2);
    const POLL: Duration = Duration::from_millis(10);
    const LIMIT: usize = super::OUTPUT_LIMIT;

    pub(crate) enum Read {
        Pending,
        Closed,
        Bytes(usize),
    }

    // Internal seam for deterministic synthetic children/clocks, not a second
    // externally configurable launcher or a caller-supplied executable boundary.
    pub(crate) trait Child {
        fn read(&mut self, stderr: bool, buffer: &mut [u8; 4096]) -> Result<Read, OperationFailed>;
        fn poll(&mut self) -> Result<Option<i32>, OperationFailed>;
        fn kill(&mut self) -> Result<(), OperationFailed>;
    }

    pub(crate) trait Backend {
        type Child: Child;
        fn spawn(
            &mut self,
            invocation: &ControllerInvocation,
        ) -> Result<Self::Child, OperationFailed>;
        fn now(&self) -> Duration;
        fn sleep(&mut self, duration: Duration);
    }

    pub(crate) struct Owner<B: Backend> {
        pub(crate) backend: B,
        retained: Option<B::Child>,
    }

    impl<B: Backend> Owner<B> {
        pub(crate) fn new(backend: B) -> Self {
            Self {
                backend,
                retained: None,
            }
        }

        fn cleanup(&mut self) {
            let Some(child) = self.retained.as_mut() else {
                return;
            };
            // Unknown is never proof of exit. Retain the owned handle on every
            // failure; future calls refuse before spawning another controller.
            if matches!(child.poll(), Ok(Some(_))) {
                self.retained = None;
                return;
            }
            let start = self.backend.now();
            let _ = child.kill();
            loop {
                if matches!(child.poll(), Ok(Some(_))) {
                    self.retained = None;
                    return;
                }
                if self.backend.now().saturating_sub(start) >= CLEANUP {
                    return;
                }
                self.backend.sleep(POLL);
            }
        }

        pub(crate) fn run(
            &mut self,
            invocation: &ControllerInvocation,
            seconds: u64,
            limit: usize,
        ) -> Result<ControllerOutput, OperationFailed> {
            if self.retained.is_some()
                || seconds != super::CONTROLLER_TIMEOUT_SECONDS
                || limit != LIMIT
            {
                return Err(OperationFailed);
            }
            let start = self.backend.now();
            self.retained = Some(self.backend.spawn(invocation)?);
            let result = self.capture(start);
            if result.is_err() {
                self.cleanup();
            }
            result
        }

        fn check_deadline(&self, start: Duration) -> Result<(), OperationFailed> {
            if self.backend.now().saturating_sub(start) >= DEADLINE {
                Err(OperationFailed)
            } else {
                Ok(())
            }
        }

        fn capture(&mut self, start: Duration) -> Result<ControllerOutput, OperationFailed> {
            let mut streams = [Vec::new(), Vec::new()];
            let mut closed = [false, false];
            let mut exit = None;
            loop {
                self.check_deadline(start)?;
                for index in 0..2 {
                    if closed[index] {
                        continue;
                    }
                    self.check_deadline(start)?;
                    let mut buffer = [0; 4096];
                    let read = self
                        .retained
                        .as_mut()
                        .ok_or(OperationFailed)?
                        .read(index == 1, &mut buffer)?;
                    // An OS call cannot be interrupted here, but once it
                    // returns its known-late result must never be admitted.
                    self.check_deadline(start)?;
                    match read {
                        Read::Pending => {}
                        Read::Closed => closed[index] = true,
                        Read::Bytes(count) => {
                            if count == 0
                                || count > buffer.len()
                                || count > LIMIT.saturating_sub(streams[index].len())
                            {
                                return Err(OperationFailed);
                            }
                            streams[index].extend_from_slice(&buffer[..count]);
                        }
                    }
                }
                // Even a flood on both streams cannot postpone the deadline or
                // exit poll. An exited parent with inherited open pipes still
                // has a finite capture deadline, with no reader-thread join.
                self.check_deadline(start)?;
                if exit.is_none() {
                    let observed = self.retained.as_mut().ok_or(OperationFailed)?.poll()?;
                    self.check_deadline(start)?;
                    exit = observed;
                }
                self.check_deadline(start)?;
                if let Some(exit_code) = exit
                    && closed == [true, true]
                {
                    self.retained = None;
                    let [stdout, stderr] = streams;
                    return Ok(ControllerOutput {
                        exit_code,
                        stdout,
                        stderr,
                    });
                }
                self.backend.sleep(POLL);
            }
        }
    }

    impl<B: Backend> Drop for Owner<B> {
        fn drop(&mut self) {
            // Last-resort direct-child request only: Drop never waits and never
            // signals runtime descendants or a PID supplied in controller JSON.
            if let Some(child) = self.retained.as_mut() {
                let _ = child.kill();
            }
        }
    }

    #[cfg(windows)]
    pub(crate) mod native {
        use super::{Backend, Child, OperationFailed, Owner, Read};
        use std::{
            process,
            time::{Duration, Instant},
        };

        pub(crate) struct NativeBackend(Instant);
        impl Default for NativeBackend {
            fn default() -> Self {
                Self(Instant::now())
            }
        }
        pub(crate) struct NativeChild(process::Child);
        impl Child for NativeChild {
            fn read(
                &mut self,
                stderr: bool,
                buffer: &mut [u8; 4096],
            ) -> Result<Read, OperationFailed> {
                use migration_stream_windows_native::{
                    ChildPipeRead, read_child_stderr, read_child_stdout,
                };
                let read = if stderr {
                    read_child_stderr(self.0.stderr.as_mut().ok_or(OperationFailed)?, buffer)
                } else {
                    read_child_stdout(self.0.stdout.as_mut().ok_or(OperationFailed)?, buffer)
                }
                .map_err(|_| OperationFailed)?;
                Ok(match read {
                    ChildPipeRead::Pending => Read::Pending,
                    ChildPipeRead::Closed => Read::Closed,
                    ChildPipeRead::Read(count) => Read::Bytes(count),
                })
            }
            fn poll(&mut self) -> Result<Option<i32>, OperationFailed> {
                self.0
                    .try_wait()
                    .map(|status| status.map(|value| value.code().unwrap_or(-1)))
                    .map_err(|_| OperationFailed)
            }
            fn kill(&mut self) -> Result<(), OperationFailed> {
                self.0.kill().map_err(|_| OperationFailed)
            }
        }
        impl Backend for NativeBackend {
            type Child = NativeChild;
            fn spawn(
                &mut self,
                invocation: &super::ControllerInvocation,
            ) -> Result<NativeChild, OperationFailed> {
                invocation
                    .command()
                    .spawn()
                    .map(NativeChild)
                    .map_err(|_| OperationFailed)
            }
            fn now(&self) -> Duration {
                self.0.elapsed()
            }
            fn sleep(&mut self, duration: Duration) {
                std::thread::sleep(duration);
            }
        }
        /// Blocking process owner. Integrations must call it off their UI thread.
        /// Only the controller is owned; runtime descendants are never killed.
        #[derive(Default)]
        pub struct WindowsProcessRunner(Option<Owner<NativeBackend>>);
        impl super::super::ControllerRunner for WindowsProcessRunner {
            fn run(
                &mut self,
                invocation: &super::ControllerInvocation,
                seconds: u64,
                limit: usize,
            ) -> Result<super::super::ControllerOutput, OperationFailed> {
                self.0
                    .get_or_insert_with(|| Owner::new(NativeBackend::default()))
                    .run(invocation, seconds, limit)
            }
        }
    }
}

#[cfg(windows)]
pub use process_owner::native::WindowsProcessRunner;

/// Fixed private failure text; no path, OS message or controller output is shown.
pub const ENGINEERING_LAUNCH_ERROR: &str = "Migration Stream could not open the selected document.";

#[derive(Debug, Eq, PartialEq)]
pub enum LauncherFailure {
    Arguments,
    Layout,
    Open(Failure),
}

/// Metadata retention boundary, not publisher authentication or atomic spawn
/// image pinning. The operator must already trust the launcher and runtime.
pub trait EngineeringLayoutChecks {
    type Held;
    fn retain(
        &mut self,
        executable: &WindowsPath,
        directory: &WindowsPath,
        runtime: &WindowsPath,
        controller: &WindowsPath,
    ) -> Result<Self::Held, OperationFailed>;
}

/// The executable and operands are native UTF-16 from the real entrypoint.
/// Unsupported current-executable namespaces refuse without prefix stripping.
pub fn open_engineering_document<R: ControllerRunner, B: Browser, L: EngineeringLayoutChecks>(
    executable_units: Vec<u16>,
    operands: Vec<Vec<u16>>,
    checks: &mut L,
    runner: R,
    browser: B,
) -> Result<(), LauncherFailure> {
    let [operand]: [Vec<u16>; 1] = operands
        .try_into()
        .map_err(|_| LauncherFailure::Arguments)?;
    let document = WindowsPath::from_units(operand).map_err(|_| LauncherFailure::Arguments)?;
    let executable =
        WindowsPath::from_units(executable_units).map_err(|_| LauncherFailure::Layout)?;
    if executable.units().get(1..3) != Some(&[58, 92]) {
        return Err(LauncherFailure::Layout); // ordinary fully qualified drive only
    }
    let separator = executable
        .units()
        .iter()
        .rposition(|unit| *unit == 92)
        .ok_or(LauncherFailure::Layout)?;
    let name: Vec<u16> = "migration-stream-document-open.exe"
        .encode_utf16()
        .collect();
    if executable.units()[separator + 1..] != name {
        return Err(LauncherFailure::Layout);
    }
    let directory = WindowsPath::from_units(executable.units()[..separator].to_vec())
        .map_err(|_| LauncherFailure::Layout)?;
    let mut units = directory.units().to_vec();
    units.extend("\\Runtime".encode_utf16());
    let runtime = WindowsPath::from_units(units).map_err(|_| LauncherFailure::Layout)?;
    let mut units = runtime.units().to_vec();
    units.extend("\\libexec\\migration-stream-local-runtime.exe".encode_utf16());
    let controller = WindowsPath::from_units(units).map_err(|_| LauncherFailure::Layout)?;
    // Keep the exact metadata handles alive through controller and presenter.
    let _held = checks
        .retain(&executable, &directory, &runtime, &controller)
        .map_err(|_| LauncherFailure::Layout)?;
    let mut coordinator = DocumentOpenCoordinator::new(runtime, runner, browser)
        .map_err(|_| LauncherFailure::Layout)?;
    coordinator
        .open_documents(&[document])
        .map_err(LauncherFailure::Open)
}

#[cfg(windows)]
mod engineering_native {
    use super::{
        Browser, BrowserDestination, EngineeringLayoutChecks, OperationFailed, WindowsPath,
    };
    use migration_stream_windows_native::{RealDirectory, RealFile};
    use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};

    pub struct WindowsEngineeringLayout;
    pub struct HeldLayout {
        _files: [RealFile; 2],
        _directories: [RealDirectory; 2],
    }
    fn path(value: &WindowsPath) -> PathBuf {
        PathBuf::from(OsString::from_wide(value.units()))
    }
    impl EngineeringLayoutChecks for WindowsEngineeringLayout {
        type Held = HeldLayout;
        fn retain(
            &mut self,
            executable: &WindowsPath,
            directory: &WindowsPath,
            runtime: &WindowsPath,
            controller: &WindowsPath,
        ) -> Result<HeldLayout, OperationFailed> {
            let files = [
                RealFile::capture(&path(executable)),
                RealFile::capture(&path(controller)),
            ];
            let [executable, controller] = files.map(|file| file.map_err(|_| OperationFailed));
            let files = [executable?, controller?];
            let directories = [
                RealDirectory::capture(&path(directory)),
                RealDirectory::capture(&path(runtime)),
            ];
            let [directory, runtime] =
                directories.map(|directory| directory.map_err(|_| OperationFailed));
            let directories = [directory?, runtime?];
            for file in &files {
                file.verify().map_err(|_| OperationFailed)?;
            }
            for directory in &directories {
                directory.verify().map_err(|_| OperationFailed)?;
            }
            Ok(HeldLayout {
                _files: files,
                _directories: directories,
            })
        }
    }
    pub struct WindowsLoopbackBrowser;
    impl Browser for WindowsLoopbackBrowser {
        fn open(&mut self, destination: BrowserDestination) -> Result<(), OperationFailed> {
            if destination.as_str() != super::LOOPBACK {
                return Err(OperationFailed);
            }
            migration_stream_windows_native::submit_fixed_owned_loopback()
                .map_err(|_| OperationFailed)
        }
    }
}
#[cfg(windows)]
pub use engineering_native::{WindowsEngineeringLayout, WindowsLoopbackBrowser};

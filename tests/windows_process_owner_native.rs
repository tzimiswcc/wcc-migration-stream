//! Synthetic Windows process proof only. This is not a product binary target.
#[cfg(windows)]
#[path = "../native/windows/document_open.rs"]
mod document_open;

fn main() {
    #[cfg(windows)]
    if fixture::run().is_err() {
        eprintln!("synthetic Windows process fixture refused or failed");
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    println!("SKIP: synthetic Windows process fixture requires Windows");
}

#[cfg(windows)]
mod fixture {
    use super::document_open::{
        Browser, BrowserDestination, ControllerInvocation, ControllerOutput, ControllerRunner,
        DocumentOpenCoordinator, Failure, OperationFailed, WindowsPath, WindowsProcessRunner,
    };
    use migration_stream_windows_native::{
        CurrentUser, KillOnCloseJob, ManagedProcess, PrivateRuntimeDirectory, RealDirectory,
        RealFile, system_root_directory,
    };
    use std::{
        cell::RefCell,
        ffi::OsString,
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::windows::{ffi::OsStringExt, fs::OpenOptionsExt},
        path::{Path, PathBuf},
        rc::Rc,
        thread,
        time::{Duration, Instant},
    };

    type Result<T> = std::result::Result<T, ()>;
    const CASES: [&str; 6] = [
        "native",
        "dual",
        "stdout-overflow",
        "stderr-overflow",
        "exit17",
        "timeout",
    ];
    const SENTINEL: &str = "MIGRATION_STREAM_SYNTHETIC_PARENT_SENTINEL";
    const HELPER: &str = "migration-stream-local-runtime.exe";
    const CAP: usize = 65_536;

    // One create-only eight-byte record per fixed stage, confined to the owned
    // synthetic case directory. Never serialize an error, path, argv or output.
    const STAGES: [&str; 9] = [
        "driver-start",
        "driver-path",
        "driver-env",
        "invocation",
        "owner-enter",
        "owner-return",
        "ready",
        "witness",
        "assertions",
    ];
    fn mark(
        root: &Path,
        case: &str,
        stage: &str,
        category: u8,
        kind: u8,
        code: Option<i32>,
    ) -> Result<()> {
        if !CASES.contains(&case) || !STAGES.contains(&stage) {
            return Err(());
        }
        let mut record = [1, category, kind, u8::from(code.is_some()), 0, 0, 0, 0];
        record[4..].copy_from_slice(&code.unwrap_or(0).to_le_bytes());
        write_new(
            &root
                .join("documents")
                .join(case)
                .join(format!("diagnostic-{stage}")),
            &record,
        )
    }
    fn kind_code(error: &std::io::Error) -> u8 {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::NotFound => 1,
            ErrorKind::PermissionDenied => 2,
            ErrorKind::AlreadyExists => 3,
            ErrorKind::InvalidInput => 4,
            ErrorKind::TimedOut => 5,
            ErrorKind::WouldBlock => 6,
            ErrorKind::Other => 7,
            ErrorKind::UnexpectedEof => 8,
            _ => 0,
        }
    }
    fn mark_result<T>(root: &Path, case: &str, stage: &str, result: Result<T>) -> Result<T> {
        mark(
            root,
            case,
            stage,
            if result.is_ok() { 1 } else { 2 },
            0,
            None,
        )?;
        result
    }
    fn mark_io<T>(root: &Path, case: &str, stage: &str, result: std::io::Result<T>) -> Result<T> {
        match result {
            Ok(value) => {
                mark(root, case, stage, 1, 0, None)?;
                Ok(value)
            }
            Err(error) => {
                mark(
                    root,
                    case,
                    stage,
                    3,
                    kind_code(&error),
                    error.raw_os_error(),
                )?;
                Err(())
            }
        }
    }
    fn outer_io(case: &str, stage: &str, error: &std::io::Error) {
        println!(
            "DIAG case={case} stage={stage} category=os-error kind={} code-present={} code={}",
            kind_code(error),
            error.raw_os_error().is_some(),
            error.raw_os_error().unwrap_or(0)
        );
    }
    fn report_markers(root: &Path, case: &str) {
        for stage in STAGES {
            let path = root
                .join("documents")
                .join(case)
                .join(format!("diagnostic-{stage}"));
            let metadata = match fs::symlink_metadata(&path) {
                Ok(value) => value,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    println!("DIAG case={case} stage={stage} category=missing");
                    continue;
                }
                Err(error) => {
                    outer_io(case, stage, &error);
                    continue;
                }
            };
            if !metadata.file_type().is_file() || metadata.len() != 8 {
                println!("DIAG case={case} stage={stage} category=invalid-marker");
                continue;
            }
            let mut bytes = Vec::new();
            let read = File::open(path).and_then(|file| file.take(9).read_to_end(&mut bytes));
            if let Err(error) = read {
                outer_io(case, stage, &error);
                continue;
            }
            if bytes.len() != 8 || bytes[0] != 1 || bytes[1] > 5 || bytes[2] > 8 || bytes[3] > 1 {
                println!("DIAG case={case} stage={stage} category=invalid-marker");
                continue;
            }
            let category = [
                "entered",
                "ok",
                "refused",
                "os-error",
                "owner-output",
                "owner-refused",
            ][usize::from(bytes[1])];
            let code = i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
            println!(
                "DIAG case={case} stage={stage} category={category} kind={} code-present={} code={code}",
                bytes[2],
                bytes[3] == 1
            );
        }
    }

    pub fn run() -> Result<()> {
        let arguments: Vec<_> = std::env::args_os().skip(1).collect();
        match arguments.as_slice() {
            [] => outer(),
            [mode, case, root] if mode == "--case" => {
                let case = case
                    .to_str()
                    .filter(|name| CASES.contains(name))
                    .ok_or(())?;
                driver(case, Path::new(root))
            }
            [open, separator, document] if open == "open" && separator == "--" => {
                helper(Path::new(document))
            }
            _ => Err(()),
        }
    }

    fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|_| ())?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| ())
    }

    fn executable_bytes() -> Result<Vec<u8>> {
        let executable = std::env::current_exe().map_err(|_| ())?;
        if fs::metadata(&executable).map_err(|_| ())?.len() > 64 * 1024 * 1024 {
            return Err(());
        }
        let mut bytes = Vec::new();
        File::open(executable)
            .map_err(|_| ())?
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(());
        }
        Ok(bytes)
    }

    fn outer() -> Result<()> {
        let temporary = tempfile::tempdir().map_err(|_| ())?;
        let result = prepare_and_run(temporary.path());
        if result.is_err() {
            // Preserve own synthetic evidence on failure; never delete beneath
            // a possibly unresolved child. Job cleanup still precedes return.
            let _preserved = temporary.keep();
            return Err(());
        }
        temporary.close().map_err(|_| ())
    }

    fn prepare_and_run(parent: &Path) -> Result<()> {
        let parent_handle = Rc::new(RealDirectory::capture(parent).map_err(|_| ())?);
        let private = PrivateRuntimeDirectory::open_or_create_child(
            parent_handle,
            CurrentUser::capture().map_err(|_| ())?,
            "synthetic-process-fixture",
        )
        .map_err(|_| ())?;
        // Keep the original ordinary drive spelling for WindowsPath; retained
        // native handles independently verify its filesystem identity.
        let root = parent.join("synthetic-process-fixture");
        WindowsPath::from_native(root.as_os_str()).map_err(|_| ())?;
        let runtime = root.join("Runtime");
        fs::create_dir(&runtime).map_err(|_| ())?;
        fs::create_dir(runtime.join("libexec")).map_err(|_| ())?;
        fs::create_dir(root.join("documents")).map_err(|_| ())?;
        let bytes = executable_bytes()?;
        let driver = root.join("synthetic-driver.exe");
        let helper = runtime.join("libexec").join(HELPER);
        write_new(&driver, &bytes)?;
        write_new(&helper, &bytes)?;
        let driver_handle = Rc::new(RealFile::capture(&driver).map_err(|_| ())?);
        let helper_handle = RealFile::capture(&helper).map_err(|_| ())?;
        for case in CASES {
            private.directory().verify().map_err(|_| ())?;
            driver_handle.verify().map_err(|_| ())?;
            helper_handle.verify().map_err(|_| ())?;
            fs::create_dir(root.join("documents").join(case)).map_err(|_| ())?;
            println!("RUN synthetic Windows process case: {case}");
            run_contained_case(case, &root, Rc::clone(&driver_handle), private.directory())?;
            if fs::read(&driver).map_err(|_| ())? != bytes
                || fs::read(&helper).map_err(|_| ())? != bytes
            {
                return Err(());
            }
            println!("PASS synthetic Windows process case: {case}");
        }
        println!("PASS: 6 synthetic Windows process cases; no product runtime");
        Ok(())
    }

    fn wait_for_exit(
        child: &ManagedProcess,
        deadline: Instant,
        case: &str,
        stage: &str,
    ) -> Result<u32> {
        loop {
            let observed = child
                .try_wait()
                .map_err(|error| outer_io(case, stage, &error))?;
            if Instant::now() >= deadline {
                println!("DIAG case={case} stage={stage} category=deadline");
                return Err(());
            }
            if let Some(code) = observed {
                println!("DIAG case={case} stage={stage} category=known-exit code={code}");
                return Ok(code);
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn run_contained_case(
        case: &str,
        root: &Path,
        executable: Rc<RealFile>,
        directory: Rc<RealDirectory>,
    ) -> Result<()> {
        // The existing native Job contract requires canonical SystemRoot.
        // Retain/replay its public capability, never ambient environment values.
        let system_root =
            system_root_directory().map_err(|error| outer_io(case, "system-root", &error))?;
        system_root
            .verify()
            .map_err(|error| outer_io(case, "system-root", &error))?;
        println!("DIAG case={case} stage=job-create category=entered");
        let job = KillOnCloseJob::new().map_err(|error| outer_io(case, "job-create", &error))?;
        println!("DIAG case={case} stage=job-create category=ok");
        let deadline = Instant::now() + Duration::from_secs(60);
        // Admission failure refuses: no unsupervised spawn fallback. The Job
        // disallows breakaway; the production Command uses no breakaway flag.
        println!("DIAG case={case} stage=job-admission category=entered");
        let child = job
            .spawn_and_assign(
                executable,
                &[
                    OsString::from("--case"),
                    OsString::from(case),
                    root.as_os_str().to_owned(),
                ],
                &[
                    (
                        OsString::from("SystemRoot"),
                        system_root.environment_path().as_os_str().to_owned(),
                    ),
                    (OsString::from(SENTINEL), OsString::from("synthetic-only")),
                ],
                directory,
            )
            .map_err(|error| outer_io(case, "job-admission", &error))?;
        println!("DIAG case={case} stage=job-admission category=ok");
        // Any failed post-admission replay goes directly to the same owned
        // Job cleanup below; never drop a running child through an early `?`.
        let outcome = system_root
            .verify()
            .map_err(|error| outer_io(case, "system-root-replay", &error))
            .and_then(|()| wait_for_exit(&child, deadline, case, "driver-exit"))
            .and_then(|code| if code == 0 { Ok(()) } else { Err(()) });
        // Driver must prove owner cleanup before exiting zero. This outer Job
        // termination occurs afterward and cannot turn a failed case into PASS.
        let terminated = job
            .terminate(91)
            .map_err(|error| outer_io(case, "job-cleanup", &error));
        let reaped = wait_for_exit(
            &child,
            Instant::now() + Duration::from_secs(5),
            case,
            "driver-reap",
        );
        report_markers(root, case);
        outcome.and(terminated).and(reaped.map(|_| ()))
    }

    fn document(root: &Path, case: &str) -> PathBuf {
        let mut filename: Vec<_> = "Synthetic π & %; ".encode_utf16().collect();
        filename.push(0xd800);
        filename.extend(".mstream".encode_utf16());
        root.join("documents")
            .join(case)
            .join(OsString::from_wide(&filename))
    }

    struct Capture(Rc<RefCell<Option<ControllerInvocation>>>);
    impl ControllerRunner for Capture {
        fn run(
            &mut self,
            call: &ControllerInvocation,
            _: u64,
            _: usize,
        ) -> std::result::Result<ControllerOutput, OperationFailed> {
            *self.0.borrow_mut() = Some(call.clone());
            Err(OperationFailed)
        }
    }
    struct NoBrowser;
    impl Browser for NoBrowser {
        fn open(
            &mut self,
            destination: BrowserDestination,
        ) -> std::result::Result<(), OperationFailed> {
            let _fixed_url = destination.as_str();
            Err(OperationFailed)
        }
    }

    fn invocation(root: &Path, case: &str) -> Result<ControllerInvocation> {
        let captured = Rc::new(RefCell::new(None));
        let mut coordinator = DocumentOpenCoordinator::new(
            WindowsPath::from_native(root.join("Runtime").as_os_str()).map_err(|_| ())?,
            Capture(Rc::clone(&captured)),
            NoBrowser,
        )
        .map_err(|_| ())?;
        let doc = WindowsPath::from_native(document(root, case).as_os_str()).map_err(|_| ())?;
        if coordinator.open_documents(std::slice::from_ref(&doc)) != Err(Failure::Controller) {
            return Err(());
        }
        let call = captured.borrow_mut().take().ok_or(())?;
        let expected = WindowsPath::from_native(
            root.join("Runtime")
                .join("libexec")
                .join(HELPER)
                .as_os_str(),
        )
        .map_err(|_| ())?;
        if call.executable() != &expected || call.arguments()[2] != doc.units() {
            return Err(());
        }
        Ok(call)
    }

    fn driver(case: &str, root: &Path) -> Result<()> {
        mark(root, case, "driver-start", 0, 0, None)?;
        mark_result(
            root,
            case,
            "driver-path",
            WindowsPath::from_native(root.as_os_str()).map_err(|_| ()),
        )?;
        let environment = if std::env::var_os(SENTINEL) == Some(OsString::from("synthetic-only")) {
            Ok(())
        } else {
            Err(())
        };
        mark_result(root, case, "driver-env", environment)?;
        let call = mark_result(root, case, "invocation", invocation(root, case))?;
        mark(root, case, "owner-enter", 0, 0, None)?;
        let start = Instant::now();
        let mut runner = WindowsProcessRunner::default();
        let result = runner.run(&call, 45, CAP);
        mark(
            root,
            case,
            "owner-return",
            if result.is_ok() { 4 } else { 5 },
            0,
            result.as_ref().ok().map(|output| output.exit_code),
        )?;
        let folder = root.join("documents").join(case);
        let ready = mark_io(root, case, "ready", fs::read(folder.join("ready")))?;
        if ready != b"held-v1" {
            return Err(());
        }
        // Helper deliberately forgets its exclusive file, with no close path.
        // Reopening before outer Job cleanup witnesses OS handle release for
        // this trusted synthetic helper; no PID lookup or PID-only kill.
        let _released = mark_io(
            root,
            case,
            "witness",
            OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(folder.join("held")),
        )?;
        // Diagnostic writes/readiness/release observation remain inside the
        // existing bound; they cannot let self-expiry masquerade as cleanup.
        let elapsed = start.elapsed();
        let assertions = assert_case(case, result, elapsed);
        mark_result(root, case, "assertions", assertions)
    }

    fn assert_case(
        case: &str,
        result: std::result::Result<ControllerOutput, OperationFailed>,
        elapsed: Duration,
    ) -> Result<()> {
        match case {
            "native" => {
                let output = result.map_err(|_| ())?;
                if output.exit_code != 0
                    || output.stdout != b"native-fixture-ok"
                    || !output.stderr.is_empty()
                {
                    return Err(());
                }
            }
            "dual" => {
                let output = result.map_err(|_| ())?;
                if output.exit_code != 0
                    || output.stdout != vec![b'o'; CAP]
                    || output.stderr != vec![b'e'; CAP]
                {
                    return Err(());
                }
            }
            "exit17" => {
                let output = result.map_err(|_| ())?;
                if output.exit_code != 17 {
                    return Err(());
                }
            }
            "stdout-overflow" | "stderr-overflow" => {
                if result.is_ok() || elapsed >= Duration::from_secs(10) {
                    return Err(());
                }
            }
            "timeout" => {
                if result.is_ok()
                    || elapsed < Duration::from_secs(45)
                    || elapsed >= Duration::from_millis(49_500)
                {
                    return Err(());
                }
            }
            _ => return Err(()),
        }
        Ok(())
    }

    fn helper(doc: &Path) -> Result<()> {
        // Every synthetic helper self-expires; no automatic rerun or external
        // executable, product runtime, network or user document is involved.
        thread::spawn(|| {
            thread::sleep(Duration::from_secs(50));
            std::process::exit(95);
        });
        let case = doc
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .ok_or(())?;
        if !CASES.contains(&case) {
            return Err(());
        }
        let root = doc
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .ok_or(())?;
        if doc != document(root, case)
            || std::env::current_exe().map_err(|_| ())?.file_name()
                != Some(std::ffi::OsStr::new(HELPER))
            || std::env::var_os(SENTINEL).is_some()
        {
            return Err(());
        }
        // Observe EOF only: the driver already has Job-provided NUL stdin.
        // Explicit stdin(null) is confirmed by unchanged command source review,
        // not uniquely discriminated from inherited NUL by this fixture.
        let mut stdin = [0];
        if std::io::stdin().read(&mut stdin).map_err(|_| ())? != 0 {
            return Err(());
        }
        let folder = doc.parent().ok_or(())?;
        let held = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(folder.join("held"))
            .map_err(|_| ())?;
        std::mem::forget(held); // OS exit is the only handle-release path
        write_new(&folder.join("ready"), b"held-v1")?;
        match case {
            "native" => std::io::stdout()
                .write_all(b"native-fixture-ok")
                .map_err(|_| ())?,
            "dual" => {
                let (out, err) = thread::scope(|scope| {
                    let out = scope.spawn(|| std::io::stdout().write_all(&vec![b'o'; CAP]));
                    let err = scope.spawn(|| std::io::stderr().write_all(&vec![b'e'; CAP]));
                    (out.join(), err.join())
                });
                out.map_err(|_| ())?.map_err(|_| ())?;
                err.map_err(|_| ())?.map_err(|_| ())?;
            }
            "exit17" => std::process::exit(17),
            "stdout-overflow" => {
                let _ = std::io::stdout().write_all(&vec![b'o'; CAP + 1]);
                park();
            }
            "stderr-overflow" => {
                let _ = std::io::stderr().write_all(&vec![b'e'; CAP + 1]);
                park();
            }
            "timeout" => park(),
            _ => return Err(()),
        }
        Ok(())
    }

    fn park() -> ! {
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }
}

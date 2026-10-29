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
        RealFile,
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

    fn wait_for_exit(child: &ManagedProcess, deadline: Instant) -> Result<u32> {
        loop {
            let observed = child.try_wait().map_err(|_| ())?;
            if Instant::now() >= deadline {
                return Err(());
            }
            if let Some(code) = observed {
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
        let job = KillOnCloseJob::new().map_err(|_| ())?;
        let deadline = Instant::now() + Duration::from_secs(60);
        // Admission failure refuses: no unsupervised spawn fallback. The Job
        // disallows breakaway; the production Command uses no breakaway flag.
        let child = job
            .spawn_and_assign(
                executable,
                &[
                    OsString::from("--case"),
                    OsString::from(case),
                    root.as_os_str().to_owned(),
                ],
                &[(OsString::from(SENTINEL), OsString::from("synthetic-only"))],
                directory,
            )
            .map_err(|_| ())?;
        let outcome = wait_for_exit(&child, deadline)
            .and_then(|code| if code == 0 { Ok(()) } else { Err(()) });
        // Driver must prove owner cleanup before exiting zero. This outer Job
        // termination occurs afterward and cannot turn a failed case into PASS.
        let terminated = job.terminate(91).map_err(|_| ());
        let reaped = wait_for_exit(&child, Instant::now() + Duration::from_secs(5));
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
        WindowsPath::from_native(root.as_os_str()).map_err(|_| ())?;
        if std::env::var_os(SENTINEL) != Some(OsString::from("synthetic-only")) {
            return Err(());
        }
        let call = invocation(root, case)?;
        let start = Instant::now();
        let mut runner = WindowsProcessRunner::default();
        let result = runner.run(&call, 45, CAP);
        let folder = root.join("documents").join(case);
        if fs::read(folder.join("ready")).map_err(|_| ())? != b"held-v1" {
            return Err(());
        }
        // Helper deliberately forgets its exclusive file, with no close path.
        // Reopening before outer Job cleanup witnesses OS handle release for
        // this trusted synthetic helper; no PID lookup or PID-only kill.
        let _released = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(folder.join("held"))
            .map_err(|_| ())?;
        // Include readiness/OS-release observation in the accepted time bound;
        // a delayed witness cannot allow 50-second self-expiry to masquerade as cleanup.
        let elapsed = start.elapsed();
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

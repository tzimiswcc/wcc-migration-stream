#[path = "../native/windows/document_open.rs"]
mod document_open;

use std::{cell::RefCell, collections::VecDeque, rc::Rc};

use document_open::{
    Browser, BrowserDestination, ControllerInvocation, ControllerOutput, ControllerRunner,
    DocumentOpenCoordinator, Failure, OperationFailed, WindowsPath,
};
use serde_json::{Value, json};

#[derive(Default)]
struct Calls {
    processes: Vec<(ControllerInvocation, u64, usize)>,
    browsers: Vec<String>,
    outputs: VecDeque<Result<ControllerOutput, OperationFailed>>,
    browser_fails: bool,
}

struct Runner(Rc<RefCell<Calls>>);
struct Presenter(Rc<RefCell<Calls>>);

impl ControllerRunner for Runner {
    fn run(
        &mut self,
        invocation: &ControllerInvocation,
        seconds: u64,
        limit: usize,
    ) -> Result<ControllerOutput, OperationFailed> {
        let mut calls = self.0.borrow_mut();
        calls.processes.push((invocation.clone(), seconds, limit));
        calls
            .outputs
            .pop_front()
            .expect("no unplanned process calls")
    }
}

impl Browser for Presenter {
    fn open(&mut self, destination: BrowserDestination) -> Result<(), OperationFailed> {
        let mut calls = self.0.borrow_mut();
        calls.browsers.push(destination.as_str().to_owned());
        if calls.browser_fails {
            Err(OperationFailed)
        } else {
            Ok(())
        }
    }
}

fn path(value: &str) -> WindowsPath {
    WindowsPath::from_units(value.encode_utf16().collect()).unwrap()
}

fn envelope() -> Value {
    json!({
        "schemaVersion": "migration-stream/assisted-local-runtime/cli/v1", "ok": true,
        "result": {
            "schemaVersion": "migration-stream/assisted-local-runtime/status/v2",
            "classification": "OWNED", "listenAddress": "127.0.0.1:4173",
            "url": "http://127.0.0.1:4173", "bundle": {}, "workspace": {}
        }
    })
}

fn output(value: &Value) -> ControllerOutput {
    ControllerOutput {
        exit_code: 0,
        stdout: serde_json::to_vec(value).unwrap(),
        stderr: vec![],
    }
}

fn fixture(
    outputs: impl IntoIterator<Item = Result<ControllerOutput, OperationFailed>>,
) -> (
    DocumentOpenCoordinator<Runner, Presenter>,
    Rc<RefCell<Calls>>,
) {
    let calls = Rc::new(RefCell::new(Calls {
        outputs: outputs.into_iter().collect(),
        ..Calls::default()
    }));
    let coordinator = DocumentOpenCoordinator::new(
        path(r"C:\Trusted Bundle\Runtime"),
        Runner(calls.clone()),
        Presenter(calls.clone()),
    )
    .unwrap();
    (coordinator, calls)
}

#[test]
fn owned_response_uses_fixed_controller_native_arguments_and_loopback() {
    let (mut coordinator, calls) = fixture([Ok(output(&envelope()))]);
    let document = path(r"C:\Synthetic & % $; projects\π customer\project.mstream");
    coordinator
        .open_documents(std::slice::from_ref(&document))
        .unwrap();
    let calls = calls.borrow();
    let (invocation, seconds, limit) = &calls.processes[0];
    assert_eq!(
        invocation.executable(),
        &path(r"C:\Trusted Bundle\Runtime\libexec\migration-stream-local-runtime.exe")
    );
    assert_eq!(
        invocation.arguments()[0],
        "open".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        invocation.arguments()[1],
        "--".encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(invocation.arguments()[2], document.units());
    assert_eq!((*seconds, *limit), (45, 65_536));
    assert_eq!(calls.browsers, ["http://127.0.0.1:4173"]);
}

#[test]
fn unpaired_native_utf16_units_remain_exact_across_reopen() {
    let mut units: Vec<_> = r"C:\Synthetic\".encode_utf16().collect();
    units.push(0xd800);
    units.extend(r"\project.mstream".encode_utf16());
    let document = WindowsPath::from_units(units.clone()).unwrap();
    let (mut coordinator, calls) = fixture([Ok(output(&envelope())), Ok(output(&envelope()))]);
    coordinator
        .open_documents(std::slice::from_ref(&document))
        .unwrap();
    coordinator.open_documents(&[document]).unwrap();
    assert!(
        calls
            .borrow()
            .processes
            .iter()
            .all(|(call, _, _)| call.arguments()[2] == units)
    );
}

#[test]
fn unc_path_is_not_converted_to_a_url_or_rewritten() {
    let document = path(r"\\synthetic-server\synthetic-share\Customer Data\project.mstream");
    let (mut coordinator, calls) = fixture([Ok(output(&envelope()))]);
    coordinator
        .open_documents(std::slice::from_ref(&document))
        .unwrap();
    assert_eq!(
        calls.borrow().processes[0].0.arguments()[2],
        document.units()
    );
}

#[test]
fn relative_device_normalized_or_option_like_paths_are_refused() {
    for value in [
        "",
        r"project.mstream",
        r"--workspace",
        r"C:project.mstream",
        r"\project.mstream",
        r"C:\",
        r"file://server/share/project.mstream",
        r"https://invalid.example/",
        r"\\?\C:\Synthetic\project.mstream",
        r"\\.\pipe\synthetic",
        r"C:\Synthetic\\project.mstream",
        r"C:\Synthetic\..\project.mstream",
        r"C:\Synthetic\.\project.mstream",
        r"C:\Synthetic\project.mstream ",
        r"C:\Synthetic\project.mstream.",
        r"C:/Synthetic/project.mstream",
        r"C:\Synthetic\project.mstream:stream",
        r"C:\Synthetic\*.mstream",
        r"C:\Synthetic\project.mstream\",
        "C:\\Synthetic\\bad\0name",
    ] {
        assert_eq!(
            WindowsPath::from_units(value.encode_utf16().collect()),
            Err(Failure::InvalidPath),
            "{value:?}"
        );
    }
}

#[test]
fn zero_or_multiple_documents_do_not_call_operations() {
    let (mut coordinator, calls) = fixture([]);
    assert_eq!(
        coordinator.open_documents(&[]),
        Err(Failure::InvalidDocument)
    );
    assert_eq!(
        coordinator.open_documents(&[
            path(r"C:\One\project.mstream"),
            path(r"C:\Two\project.mstream")
        ]),
        Err(Failure::InvalidDocument)
    );
    assert!(calls.borrow().processes.is_empty() && calls.borrow().browsers.is_empty());
}

#[test]
fn successful_document_identity_is_pinned_without_case_or_path_folding() {
    let (mut coordinator, calls) = fixture([Ok(output(&envelope()))]);
    coordinator
        .open_documents(&[path(r"C:\One\project.mstream")])
        .unwrap();
    for changed in [r"C:\Two\project.mstream", r"c:\One\project.mstream"] {
        assert_eq!(
            coordinator.open_documents(&[path(changed)]),
            Err(Failure::DifferentDocument)
        );
    }
    assert_eq!(calls.borrow().processes.len(), 1);
}

#[test]
fn browser_failure_retains_validated_document_pin() {
    let (mut coordinator, calls) = fixture([Ok(output(&envelope())), Ok(output(&envelope()))]);
    calls.borrow_mut().browser_fails = true;
    let document = path(r"C:\One\project.mstream");
    assert_eq!(
        coordinator.open_documents(std::slice::from_ref(&document)),
        Err(Failure::Browser)
    );
    assert_eq!(
        coordinator.open_documents(&[path(r"C:\Two\project.mstream")]),
        Err(Failure::DifferentDocument)
    );
    calls.borrow_mut().browser_fails = false;
    coordinator.open_documents(&[document]).unwrap();
    assert_eq!(calls.borrow().processes.len(), 2);
}

#[test]
fn runner_failure_does_not_pin_or_present_and_has_no_automatic_retry() {
    let (mut coordinator, calls) = fixture([Err(OperationFailed), Ok(output(&envelope()))]);
    assert_eq!(
        coordinator.open_documents(&[path(r"C:\One\project.mstream")]),
        Err(Failure::Controller)
    );
    assert_eq!(calls.borrow().processes.len(), 1);
    assert_eq!(calls.borrow().browsers, Vec::<String>::new());
    coordinator
        .open_documents(&[path(r"C:\Two\project.mstream")])
        .unwrap();
}

#[test]
fn nonzero_controller_exit_never_opens_browser() {
    let mut failed = output(&envelope());
    failed.exit_code = 1;
    let (mut coordinator, calls) = fixture([Ok(failed)]);
    assert_eq!(
        coordinator.open_documents(&[path(r"C:\One\project.mstream")]),
        Err(Failure::Controller)
    );
    assert_eq!(calls.borrow().browsers, Vec::<String>::new());
}

#[test]
fn exact_owned_envelope_is_required_before_browser_presentation() {
    for (field, value) in [
        ("schemaVersion", "wrong"),
        ("classification", "FOREIGN"),
        ("classification", "STOPPED"),
        ("classification", "STALE"),
        ("listenAddress", "0.0.0.0:4173"),
        ("listenAddress", "127.0.0.1:4174"),
        ("url", "http://localhost:4173"),
        ("url", "http://127.0.0.1:4173/"),
        ("url", "https://127.0.0.1:4173"),
        ("url", "file:///C:/Synthetic"),
        ("url", "http://127.0.0.1:4173@invalid.example"),
    ] {
        let mut wrong = envelope();
        wrong["result"][field] = json!(value);
        let (mut coordinator, calls) = fixture([Ok(output(&wrong))]);
        assert_eq!(
            coordinator.open_documents(&[path(r"C:\One\project.mstream")]),
            Err(Failure::InvalidResponse)
        );
        assert_eq!(calls.borrow().browsers, Vec::<String>::new());
    }
}

#[test]
fn unsuccessful_or_wrong_outer_envelope_never_pins() {
    for change in [json!(false), json!("true")] {
        let mut wrong = envelope();
        wrong["ok"] = change;
        let (mut coordinator, calls) = fixture([Ok(output(&wrong)), Ok(output(&envelope()))]);
        assert_eq!(
            coordinator.open_documents(&[path(r"C:\One\project.mstream")]),
            Err(Failure::InvalidResponse)
        );
        assert_eq!(calls.borrow().browsers, Vec::<String>::new());
        coordinator
            .open_documents(&[path(r"C:\Two\project.mstream")])
            .unwrap();
    }
    let mut wrong = envelope();
    wrong["schemaVersion"] = json!("wrong");
    let (mut coordinator, _) = fixture([Ok(output(&wrong))]);
    assert_eq!(
        coordinator.open_documents(&[path(r"C:\One\project.mstream")]),
        Err(Failure::InvalidResponse)
    );
}

#[test]
fn malformed_duplicate_or_trailing_json_is_refused() {
    let valid = serde_json::to_string(&envelope()).unwrap();
    let duplicate = valid.replacen("\"ok\":true", "\"ok\":false,\"ok\":true", 1);
    for stdout in [
        vec![0xff],
        b"{}".to_vec(),
        format!("{valid}{{}}").into_bytes(),
        duplicate.into_bytes(),
    ] {
        let (mut coordinator, calls) = fixture([Ok(ControllerOutput {
            exit_code: 0,
            stdout,
            stderr: vec![],
        })]);
        assert_eq!(
            coordinator.open_documents(&[path(r"C:\One\project.mstream")]),
            Err(Failure::InvalidResponse)
        );
        assert_eq!(calls.borrow().browsers, Vec::<String>::new());
    }
}

#[test]
fn either_oversized_stream_is_refused() {
    for stderr_large in [false, true] {
        let mut response = output(&envelope());
        if stderr_large {
            response.stderr = vec![b'x'; 65_537];
        } else {
            response.stdout.resize(65_537, b' ');
        }
        let (mut coordinator, calls) = fixture([Ok(response)]);
        assert_eq!(
            coordinator.open_documents(&[path(r"C:\One\project.mstream")]),
            Err(Failure::InvalidResponse)
        );
        assert_eq!(calls.borrow().browsers, Vec::<String>::new());
    }
}

#[test]
fn exact_stream_limit_is_accepted_with_valid_json() {
    let mut response = output(&envelope());
    response.stdout.resize(65_536, b' ');
    response.stderr.resize(65_536, b'x');
    let (mut coordinator, calls) = fixture([Ok(response)]);
    coordinator
        .open_documents(&[path(r"C:\One\project.mstream")])
        .unwrap();
    assert_eq!(calls.borrow().browsers.len(), 1);
}

#[test]
fn appended_fixed_controller_path_must_fit_native_path_bound() {
    let mut units: Vec<_> = r"C:\".encode_utf16().collect();
    units.resize(32_000, u16::from(b'x'));
    let root = WindowsPath::from_units(units).unwrap();
    let calls = Rc::new(RefCell::new(Calls::default()));
    assert!(matches!(
        DocumentOpenCoordinator::new(root, Runner(calls.clone()), Presenter(calls.clone())),
        Err(Failure::InvalidPath)
    ));
    assert_eq!(
        calls.borrow().processes,
        Vec::<(ControllerInvocation, u64, usize)>::new()
    );
}

#[cfg(windows)]
mod native_projection {
    use std::{
        ffi::OsString,
        os::windows::ffi::{OsStrExt, OsStringExt},
    };

    use super::{WindowsPath, envelope, fixture, output};

    #[test]
    fn native_os_paths_retain_drive_unc_and_unpaired_utf16_units() {
        for prefix in [r"C:\Synthetic & % customer\", r"\\server\share\Synthetic "] {
            let mut units: Vec<_> = prefix.encode_utf16().collect();
            units.push(0xd800);
            units.extend(r"\project.mstream".encode_utf16());
            let native = OsString::from_wide(&units);
            let selected = WindowsPath::from_native(native.as_os_str()).unwrap();
            assert_eq!(selected.units(), units);
        }
    }

    #[test]
    fn command_projection_keeps_the_fixed_program_and_three_native_arguments() {
        let mut units: Vec<_> = r"C:\Synthetic & % customer\".encode_utf16().collect();
        units.push(0xdfff);
        units.extend(r"\project.mstream".encode_utf16());
        let document = WindowsPath::from_native(OsString::from_wide(&units).as_os_str()).unwrap();
        let (mut coordinator, calls) = fixture([Ok(output(&envelope()))]);
        coordinator.open_documents(&[document]).unwrap();
        let calls = calls.borrow();
        let invocation = &calls.processes[0].0;
        // Construct/read only. Never spawn, output, status or wait on this builder.
        let command = invocation.command();
        assert_eq!(
            command.get_program().encode_wide().collect::<Vec<_>>(),
            invocation.executable().units()
        );
        let arguments: Vec<Vec<u16>> = command
            .get_args()
            .map(|argument| argument.encode_wide().collect())
            .collect();
        assert_eq!(arguments, invocation.arguments());
        assert_eq!(arguments[2], units);
        assert_eq!(command.get_envs().count(), 0);
        assert_eq!(command.get_current_dir(), None);
        // Stable Command has no inheritance-clear/stdin getter. Their unchanged
        // env_clear/null-stdin/piped policy needs static review, not a spawn probe.
    }
}

// Pure process-owner fixtures. No std::process::Command is spawned by these tests.
mod process_owner_tests {
    use super::*;
    use document_open::process_owner::{Backend, Child, Owner, Read};
    use std::{cell::Cell, time::Duration};

    #[derive(Clone, Copy)]
    enum Event {
        Pending,
        Closed,
        Bytes(usize),
        Error,
    }
    struct FakeChild {
        streams: [VecDeque<Event>; 2],
        fallback: Event,
        exit: Result<Option<i32>, OperationFailed>,
        killed_exit: bool,
        state: Rc<RefCell<State>>,
        elapsed: Rc<Cell<Duration>>,
        read_delay: [Duration; 2],
        poll_delay: Duration,
    }
    #[derive(Default)]
    struct State {
        spawns: usize,
        kills: usize,
        reads: [usize; 2],
        invocations: Vec<ControllerInvocation>,
    }
    struct FakeBackend {
        elapsed: Rc<Cell<Duration>>,
        spawn_delay: Duration,
        state: Rc<RefCell<State>>,
        child: Option<FakeChild>,
    }
    impl Child for FakeChild {
        fn read(&mut self, stderr: bool, buffer: &mut [u8; 4096]) -> Result<Read, OperationFailed> {
            let index = usize::from(stderr);
            self.elapsed
                .set(self.elapsed.get() + std::mem::take(&mut self.read_delay[index]));
            self.state.borrow_mut().reads[index] += 1;
            match self.streams[index].pop_front().unwrap_or(self.fallback) {
                Event::Pending => Ok(Read::Pending),
                Event::Closed => Ok(Read::Closed),
                Event::Error => Err(OperationFailed),
                Event::Bytes(count) => {
                    buffer.fill(if stderr { b'e' } else { b'o' });
                    Ok(Read::Bytes(count))
                }
            }
        }
        fn poll(&mut self) -> Result<Option<i32>, OperationFailed> {
            self.elapsed
                .set(self.elapsed.get() + std::mem::take(&mut self.poll_delay));
            self.exit
        }
        fn kill(&mut self) -> Result<(), OperationFailed> {
            self.state.borrow_mut().kills += 1;
            if self.killed_exit {
                self.exit = Ok(Some(1));
                Ok(())
            } else {
                Err(OperationFailed)
            }
        }
    }
    impl Backend for FakeBackend {
        type Child = FakeChild;
        fn spawn(
            &mut self,
            invocation: &ControllerInvocation,
        ) -> Result<FakeChild, OperationFailed> {
            self.elapsed.set(self.elapsed.get() + self.spawn_delay);
            self.state.borrow_mut().spawns += 1;
            self.state.borrow_mut().invocations.push(invocation.clone());
            self.child.take().ok_or(OperationFailed)
        }
        fn now(&self) -> Duration {
            self.elapsed.get()
        }
        fn sleep(&mut self, duration: Duration) {
            self.elapsed.set(self.elapsed.get() + duration);
        }
    }
    fn fixture(
        streams: [Vec<Event>; 2],
        fallback: Event,
        exit: Result<Option<i32>, OperationFailed>,
        killed_exit: bool,
    ) -> (Owner<FakeBackend>, Rc<RefCell<State>>, ControllerInvocation) {
        let (mut coordinator, calls) = super::fixture([Ok(output(&envelope()))]);
        let mut units: Vec<u16> = r"C:\Synthetic & %;\".encode_utf16().collect();
        units.push(0xd800);
        units.extend(r"\project.mstream".encode_utf16());
        coordinator
            .open_documents(&[WindowsPath::from_units(units).unwrap()])
            .unwrap();
        let invocation = calls.borrow().processes[0].0.clone();
        let state = Rc::new(RefCell::new(State::default()));
        let elapsed = Rc::new(Cell::new(Duration::ZERO));
        let child = FakeChild {
            elapsed: elapsed.clone(),
            read_delay: [Duration::ZERO; 2],
            poll_delay: Duration::ZERO,
            streams: streams.map(VecDeque::from),
            fallback,
            exit,
            killed_exit,
            state: state.clone(),
        };
        (
            Owner::new(FakeBackend {
                elapsed,
                spawn_delay: Duration::ZERO,
                state: state.clone(),
                child: Some(child),
            }),
            state,
            invocation,
        )
    }
    fn run(
        owner: &mut Owner<FakeBackend>,
        invocation: &ControllerInvocation,
    ) -> Result<ControllerOutput, OperationFailed> {
        owner.run(invocation, 45, 65_536)
    }
    #[test]
    fn owner_dual_stream_capture_and_native_invocation_are_exact() {
        let (mut owner, state, invocation) = fixture(
            [
                vec![Event::Bytes(3), Event::Closed],
                vec![Event::Bytes(4), Event::Closed],
            ],
            Event::Pending,
            Ok(Some(0)),
            true,
        );
        let result = run(&mut owner, &invocation).unwrap();
        assert_eq!(
            (result.exit_code, result.stdout, result.stderr),
            (0, b"ooo".to_vec(), b"eeee".to_vec())
        );
        assert!(invocation.arguments()[2].contains(&0xd800));
        assert_eq!(state.borrow().invocations, [invocation]);
        assert_eq!(state.borrow().kills, 0);
    }
    #[test]
    fn owner_nonzero_exit_preserves_output_for_coordinator_refusal() {
        let (mut owner, state, invocation) =
            fixture([vec![], vec![]], Event::Closed, Ok(Some(17)), true);
        assert_eq!(run(&mut owner, &invocation).unwrap().exit_code, 17);
        assert_eq!(state.borrow().kills, 0);
    }
    #[test]
    fn owner_exact_stream_caps_are_allowed() {
        let bytes = vec![Event::Bytes(4096); 16];
        let (mut owner, _, invocation) =
            fixture([bytes.clone(), bytes], Event::Closed, Ok(Some(0)), true);
        let out = run(&mut owner, &invocation).unwrap();
        assert_eq!((out.stdout.len(), out.stderr.len()), (65_536, 65_536));
    }
    #[test]
    fn owner_overflow_in_either_stream_cleans_direct_child() {
        for stream in 0..2 {
            let mut streams = [vec![], vec![]];
            streams[stream] = vec![Event::Bytes(4096); 17];
            let (mut owner, state, invocation) = fixture(streams, Event::Pending, Ok(None), true);
            assert!(run(&mut owner, &invocation).is_err());
            assert_eq!(state.borrow().kills, 1);
            assert!(owner.backend.elapsed.get() < Duration::from_secs(1));
        }
    }
    #[test]
    fn owner_both_stream_floods_remain_fair_and_bounded() {
        let (mut owner, state, invocation) =
            fixture([vec![], vec![]], Event::Bytes(4096), Ok(None), true);
        assert!(run(&mut owner, &invocation).is_err());
        assert!(state.borrow().reads.iter().all(|n| *n <= 17));
        assert_eq!(state.borrow().kills, 1);
    }
    #[test]
    fn owner_deadline_requires_exit_even_after_both_pipes_close() {
        let (mut owner, state, invocation) =
            fixture([vec![], vec![]], Event::Closed, Ok(None), true);
        assert!(run(&mut owner, &invocation).is_err());
        assert_eq!(owner.backend.elapsed.get(), Duration::from_secs(45));
        assert_eq!(state.borrow().kills, 1);
    }
    #[test]
    fn owner_inherited_open_pipes_cannot_hang_after_parent_exit() {
        let (mut owner, state, invocation) =
            fixture([vec![], vec![]], Event::Pending, Ok(Some(0)), true);
        assert!(run(&mut owner, &invocation).is_err());
        assert_eq!(owner.backend.elapsed.get(), Duration::from_secs(45));
        assert_eq!(state.borrow().kills, 0);
    }
    #[test]
    fn owner_read_and_unknown_poll_errors_never_become_success() {
        for poll_error in [false, true] {
            let (mut owner, state, invocation) = fixture(
                [
                    vec![if poll_error {
                        Event::Closed
                    } else {
                        Event::Error
                    }],
                    vec![Event::Closed],
                ],
                Event::Pending,
                if poll_error {
                    Err(OperationFailed)
                } else {
                    Ok(None)
                },
                true,
            );
            assert!(run(&mut owner, &invocation).is_err());
            assert_eq!(state.borrow().kills, 1);
        }
    }
    #[test]
    fn owner_failed_cleanup_retains_authority_and_refuses_new_spawn() {
        let (mut owner, state, invocation) = fixture(
            [vec![], vec![]],
            Event::Pending,
            Err(OperationFailed),
            false,
        );
        assert!(run(&mut owner, &invocation).is_err());
        assert_eq!(owner.backend.elapsed.get(), Duration::from_secs(2));
        assert!(run(&mut owner, &invocation).is_err());
        assert_eq!(state.borrow().spawns, 1);
        assert_eq!(state.borrow().kills, 1);
        drop(owner);
        assert_eq!(state.borrow().kills, 2); // last direct-handle request; no wait
    }
    #[test]
    fn owner_live_child_after_deadline_has_only_two_seconds_cleanup() {
        let (mut owner, state, invocation) =
            fixture([vec![], vec![]], Event::Pending, Ok(None), false);
        assert!(run(&mut owner, &invocation).is_err());
        assert_eq!(owner.backend.elapsed.get(), Duration::from_secs(47));
        assert!(run(&mut owner, &invocation).is_err());
        assert_eq!((state.borrow().spawns, state.borrow().kills), (1, 1));
    }

    #[test]
    fn owner_spawn_failure_and_wrong_limits_do_not_retry() {
        let (mut owner, state, invocation) =
            fixture([vec![], vec![]], Event::Closed, Ok(Some(0)), true);
        for (seconds, limit) in [(0, 65_536), (46, 65_536), (45, 65_537), (45, 0)] {
            assert!(owner.run(&invocation, seconds, limit).is_err());
        }
        assert_eq!(state.borrow().spawns, 0);
        owner.backend.child = None;
        assert!(run(&mut owner, &invocation).is_err());
        assert_eq!(state.borrow().spawns, 1);
        assert_eq!(state.borrow().kills, 0);
    }
    #[test]
    fn owner_invalid_reader_count_cannot_index_or_allocate_outside_cap() {
        for count in [0, 4097, usize::MAX] {
            let (mut owner, state, invocation) = fixture(
                [vec![Event::Bytes(count)], vec![]],
                Event::Pending,
                Ok(None),
                true,
            );
            assert!(run(&mut owner, &invocation).is_err());
            assert_eq!(state.borrow().kills, 1);
        }
    }
    #[test]
    fn owner_late_read_result_refuses_before_next_capture_operation() {
        for stream in 0..2 {
            for seconds in [45, 46] {
                let (mut owner, state, invocation) =
                    fixture([vec![], vec![]], Event::Closed, Ok(Some(0)), true);
                owner.backend.child.as_mut().unwrap().read_delay[stream] =
                    Duration::from_secs(seconds);
                assert!(run(&mut owner, &invocation).is_err());
                assert_eq!(
                    state.borrow().reads,
                    if stream == 0 { [1, 0] } else { [1, 1] }
                );
                assert_eq!(state.borrow().kills, 0); // cleanup observes the already-exited child
            }
        }
    }

    #[test]
    fn owner_late_exit_poll_result_cannot_be_success() {
        for seconds in [45, 46] {
            let (mut owner, state, invocation) =
                fixture([vec![], vec![]], Event::Closed, Ok(Some(0)), true);
            owner.backend.child.as_mut().unwrap().poll_delay = Duration::from_secs(seconds);
            assert!(run(&mut owner, &invocation).is_err());
            assert_eq!(state.borrow().reads, [1, 1]);
            assert_eq!(state.borrow().kills, 0);
        }
    }

    #[test]
    fn owner_slow_spawn_counts_against_capture_budget() {
        for seconds in [45, 46] {
            let (mut owner, state, invocation) =
                fixture([vec![], vec![]], Event::Closed, Ok(None), true);
            owner.backend.spawn_delay = Duration::from_secs(seconds);
            assert!(run(&mut owner, &invocation).is_err());
            assert_eq!(state.borrow().reads, [0, 0]);
            assert_eq!((state.borrow().spawns, state.borrow().kills), (1, 1));
        }
    }

    #[test]
    fn owner_delayed_calls_within_budget_still_complete() {
        let (mut owner, state, invocation) =
            fixture([vec![], vec![]], Event::Closed, Ok(Some(0)), true);
        owner.backend.spawn_delay = Duration::from_secs(10);
        let child = owner.backend.child.as_mut().unwrap();
        child.read_delay = [Duration::from_secs(10); 2];
        child.poll_delay = Duration::from_millis(14_999);
        assert_eq!(run(&mut owner, &invocation).unwrap().exit_code, 0);
        assert_eq!(owner.backend.elapsed.get(), Duration::from_millis(44_999));
        assert_eq!(state.borrow().kills, 0);
    }

    #[cfg(windows)]
    #[test]
    fn owner_native_owner_construction_has_no_process_side_effect() {
        let _coordinator = DocumentOpenCoordinator::new(
            path(r"C:\Synthetic Runtime"),
            document_open::WindowsProcessRunner::default(),
            Presenter(Rc::new(RefCell::new(Calls::default()))),
        )
        .unwrap();
    }
}

mod engineering_composition_tests {
    use super::*;
    use document_open::{EngineeringLayoutChecks, LauncherFailure, open_engineering_document};
    use std::cell::Cell;

    struct Held(Rc<Cell<bool>>);
    impl Drop for Held {
        fn drop(&mut self) {
            self.0.set(false);
        }
    }
    struct Checks {
        held: Rc<Cell<bool>>,
        paths: Vec<[WindowsPath; 4]>,
        fail: bool,
    }
    impl EngineeringLayoutChecks for Checks {
        type Held = Held;
        fn retain(
            &mut self,
            exe: &WindowsPath,
            dir: &WindowsPath,
            runtime: &WindowsPath,
            controller: &WindowsPath,
        ) -> Result<Held, OperationFailed> {
            self.paths.push([
                exe.clone(),
                dir.clone(),
                runtime.clone(),
                controller.clone(),
            ]);
            if self.fail {
                return Err(OperationFailed);
            }
            self.held.set(true);
            Ok(Held(self.held.clone()))
        }
    }
    struct CheckedRunner {
        runner: Runner,
        held: Rc<Cell<bool>>,
    }
    impl ControllerRunner for CheckedRunner {
        fn run(
            &mut self,
            invocation: &ControllerInvocation,
            seconds: u64,
            limit: usize,
        ) -> Result<ControllerOutput, OperationFailed> {
            assert!(self.held.get());
            self.runner.run(invocation, seconds, limit)
        }
    }
    struct CheckedBrowser {
        browser: Presenter,
        held: Rc<Cell<bool>>,
    }
    impl Browser for CheckedBrowser {
        fn open(&mut self, destination: BrowserDestination) -> Result<(), OperationFailed> {
            assert!(self.held.get());
            self.browser.open(destination)
        }
    }
    fn units(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }
    fn launch(
        exe: Vec<u16>,
        documents: Vec<Vec<u16>>,
        checks: &mut Checks,
        calls: &Rc<RefCell<Calls>>,
    ) -> Result<(), LauncherFailure> {
        open_engineering_document(
            exe,
            documents,
            checks,
            CheckedRunner {
                runner: Runner(calls.clone()),
                held: checks.held.clone(),
            },
            CheckedBrowser {
                browser: Presenter(calls.clone()),
                held: checks.held.clone(),
            },
        )
    }
    fn setup() -> (Checks, Rc<RefCell<Calls>>) {
        (
            Checks {
                held: Rc::new(Cell::new(false)),
                paths: vec![],
                fail: false,
            },
            Rc::new(RefCell::new(Calls {
                outputs: VecDeque::from([Ok(output(&envelope()))]),
                ..Calls::default()
            })),
        )
    }
    fn executable() -> Vec<u16> {
        units(r"C:\Engineering & % π\migration-stream-document-open.exe")
    }
    fn document() -> Vec<u16> {
        let mut value = units(r"D:\Synthetic & % π\");
        value.push(0xd800);
        value.extend(units(r"\project.mstream"));
        value
    }
    #[test]
    fn composition_uses_fixed_adjacent_runtime_and_holds_metadata_through_browser() {
        let (mut checks, calls) = setup();
        let doc = document();
        launch(executable(), vec![doc.clone()], &mut checks, &calls).unwrap();
        assert_eq!(
            checks.paths,
            [[
                path(r"C:\Engineering & % π\migration-stream-document-open.exe"),
                path(r"C:\Engineering & % π"),
                path(r"C:\Engineering & % π\Runtime"),
                path(r"C:\Engineering & % π\Runtime\libexec\migration-stream-local-runtime.exe")
            ]]
        );
        let calls = calls.borrow();
        assert_eq!(calls.processes.len(), 1);
        assert_eq!(calls.processes[0].0.arguments()[2], doc);
        assert_eq!(calls.processes[0].0.executable(), &checks.paths[0][3]);
        assert_eq!(calls.browsers, ["http://127.0.0.1:4173"]);
        assert!(!checks.held.get());
    }
    #[test]
    fn composition_bad_cardinality_flags_and_documents_refuse_before_metadata() {
        for documents in [
            vec![],
            vec![document(), document()],
            vec![units("--port")],
            vec![units("--workspace")],
            vec![units("relative.mstream")],
            vec![units(r"\\?\C:\Synthetic\project.mstream")],
        ] {
            let (mut checks, calls) = setup();
            assert_eq!(
                launch(executable(), documents, &mut checks, &calls),
                Err(LauncherFailure::Arguments)
            );
            assert_eq!(checks.paths.len(), 0);
            assert_eq!(calls.borrow().processes.len(), 0);
            assert_eq!(calls.borrow().browsers.len(), 0);
        }
    }
    #[test]
    fn composition_executable_namespace_and_basename_refuse_without_fallback() {
        for exe in [
            r"relative.exe",
            r"C:launcher.exe",
            r"\\server\share\migration-stream-document-open.exe",
            r"\\?\C:\Engineering\migration-stream-document-open.exe",
            r"\\.\C:\Engineering\migration-stream-document-open.exe",
            r"C:\Engineering\different.exe",
            r"C:\Engineering\..\migration-stream-document-open.exe",
            r"C:\migration-stream-document-open.exe",
        ] {
            let (mut checks, calls) = setup();
            assert_eq!(
                launch(units(exe), vec![document()], &mut checks, &calls),
                Err(LauncherFailure::Layout)
            );
            assert_eq!(checks.paths.len(), 0);
            assert_eq!(calls.borrow().processes.len(), 0);
            assert_eq!(calls.borrow().browsers.len(), 0);
        }
    }
    #[test]
    fn composition_failed_layout_retention_cannot_launch_or_present() {
        let (mut checks, calls) = setup();
        checks.fail = true;
        assert_eq!(
            launch(executable(), vec![document()], &mut checks, &calls),
            Err(LauncherFailure::Layout)
        );
        assert_eq!(checks.paths.len(), 1);
        assert!(!checks.held.get());
        assert_eq!(calls.borrow().processes.len(), 0);
        assert_eq!(calls.borrow().browsers.len(), 0);
    }
    #[test]
    fn composition_transport_status_and_loopback_failures_never_present() {
        for failure in 0..4 {
            let (mut checks, calls) = setup();
            let mut value = envelope();
            let response = match failure {
                0 => Err(OperationFailed),
                1 => {
                    value["result"]["classification"] = json!("FOREIGN");
                    Ok(output(&value))
                }
                2 => {
                    value["result"]["url"] = json!("http://localhost:4173");
                    Ok(output(&value))
                }
                _ => {
                    let mut output = output(&value);
                    output.exit_code = 1;
                    Ok(output)
                }
            };
            calls.borrow_mut().outputs = VecDeque::from([response]);
            assert!(matches!(
                launch(executable(), vec![document()], &mut checks, &calls),
                Err(LauncherFailure::Open(_))
            ));
            assert_eq!(calls.borrow().processes.len(), 1);
            assert_eq!(calls.borrow().browsers.len(), 0);
            assert!(!checks.held.get());
        }
    }
    #[test]
    fn composition_browser_submission_failure_is_private_and_releases_metadata() {
        let (mut checks, calls) = setup();
        calls.borrow_mut().browser_fails = true;
        assert_eq!(
            launch(executable(), vec![document()], &mut checks, &calls),
            Err(LauncherFailure::Open(Failure::Browser))
        );
        assert_eq!(calls.borrow().browsers, ["http://127.0.0.1:4173"]);
        assert!(!checks.held.get());
        assert_eq!(
            document_open::ENGINEERING_LAUNCH_ERROR,
            "Migration Stream could not open the selected document."
        );
    }
    #[cfg(windows)]
    #[test]
    fn composition_native_bindings_construct_without_metadata_or_shell_calls() {
        let _checks = document_open::WindowsEngineeringLayout;
        let _presenter = document_open::WindowsLoopbackBrowser;
    }
}

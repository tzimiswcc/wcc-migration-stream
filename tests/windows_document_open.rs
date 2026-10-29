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

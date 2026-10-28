import AppKit
import Foundation

private actor RunnerSpy {
  struct Call: Sendable {
    let executable: URL
    let arguments: [String]
  }
  var calls: [Call] = []
  var output: DocumentOpenAdapter.Output
  var fails: Bool
  let held: Bool
  var release: CheckedContinuation<Void, Never>?
  init(_ output: DocumentOpenAdapter.Output, fails: Bool = false, held: Bool = false) {
    self.output = output
    self.fails = fails
    self.held = held
  }
  func invoke(_ executable: URL, _ arguments: [String]) async throws -> DocumentOpenAdapter.Output {
    calls.append(Call(executable: executable, arguments: arguments))
    if held { await withCheckedContinuation { release = $0 } }
    if fails { throw DocumentOpenAdapter.Failure.controller }
    return output
  }
  func unblock() {
    release?.resume()
    release = nil
  }
  func succeed(_ next: DocumentOpenAdapter.Output) {
    output = next
    fails = false
  }

}

@MainActor private final class Presentation {
  var browsers: [URL] = []
  var errors: [DocumentOpenAdapter.Failure] = []
  var failBrowser = false
}

@main struct DocumentOpenAdapterTests {
  @MainActor static func expect(_ condition: @autoclosure () -> Bool, _ message: String) {
    precondition(condition(), message)
  }
  static func response(_ changes: [String: String] = [:], ok: Bool = true, status: Int32 = 0)
    -> DocumentOpenAdapter.Output
  {
    let result = [
      "schemaVersion": changes["statusSchema"]
        ?? "migration-stream/assisted-local-runtime/status/v2",
      "classification": changes["classification"] ?? "OWNED",
      "listenAddress": changes["listenAddress"] ?? "127.0.0.1:4173",
      "url": changes["url"] ?? "http://127.0.0.1:4173",
    ]
    let value: [String: Any] = [
      "schemaVersion": changes["cliSchema"] ?? "migration-stream/assisted-local-runtime/cli/v1",
      "ok": ok, "result": result,
    ]
    return .init(
      status: status, stdout: try! JSONSerialization.data(withJSONObject: value), stderr: Data())
  }
  @MainActor private static func fixture(
    _ output: DocumentOpenAdapter.Output = response(), fails: Bool = false, held: Bool = false
  ) throws -> (DocumentOpenAdapter, RunnerSpy, Presentation) {
    let spy = RunnerSpy(output, fails: fails, held: held)
    let presentation = Presentation()
    let adapter = try DocumentOpenAdapter(
      trustedRuntimeRoot: URL(fileURLWithPath: "/trusted runtime/πελάτης%literal"),
      runner: { executable, arguments in try await spy.invoke(executable, arguments) },
      browser: { url in
        if presentation.failBrowser { throw DocumentOpenAdapter.Failure.browser }
        presentation.browsers.append(url)
      },
      errorPresenter: { presentation.errors.append($0) }
    )
    return (adapter, spy, presentation)
  }
  @MainActor static func open(_ adapter: DocumentOpenAdapter, _ urls: [URL]) async {
    if let task = adapter.handleOpen(urls) { await task.value }
  }

  @MainActor static func main() async throws {
    // The test executable never constructs NSApplication, launches a process,
    // invokes NSWorkspace, registers documents, or touches an actual workspace.
    for path in [
      "/synthetic/customer readiness/project.mstream", "/synthetic/πελάτης/project.mstream",
      "/synthetic/percent%20literal/project.mstream",
      "/synthetic/$(touch nothing); 'quoted'/project.mstream",
    ] {
      let (adapter, spy, view) = try fixture()
      await open(adapter, [URL(fileURLWithPath: path)])
      let calls = await spy.calls
      expect(calls.count == 1, "one process request")
      expect(
        calls[0].executable.path
          == "/trusted runtime/πελάτης%literal/libexec/migration-stream-local-runtime",
        "fixed trusted executable")
      expect(calls[0].arguments == ["open", "--", path], "exact path argv, no shell or override")
      expect(
        view.browsers.map(\.absoluteString) == ["http://127.0.0.1:4173"],
        "validated loopback presentation")
      expect(view.errors.isEmpty, "no successful event error")
    }
    print("PASS exact native path/executable/argv and validated presentation")

    for urls in [[], [URL(fileURLWithPath: "/one"), URL(fileURLWithPath: "/two")]] {
      let (adapter, spy, view) = try fixture()
      await open(adapter, urls)
      let calls = await spy.calls
      expect(
        calls.isEmpty && view.browsers.isEmpty && view.errors == [.invalidDocument],
        "cardinality refusal")
    }
    print("PASS one document per event")

    for text in [
      "https://example.invalid/project.mstream", "file://remote/synthetic/project.mstream",
      "file://localhost/synthetic/project.mstream", "file:///synthetic/project.mstream?x=1",
      "file:///synthetic/project.mstream#fragment", "file:///synthetic/%FF/project.mstream",
      "file:///synthetic/%00/project.mstream", "relative/project.mstream",
    ] {
      let (adapter, spy, view) = try fixture()
      await open(adapter, [URL(string: text)!])
      let calls = await spy.calls
      expect(
        calls.isEmpty && view.browsers.isEmpty && view.errors == [.invalidDocument],
        "unsafe/lossy URL refusal: \(text)")
    }
    print("PASS remote/query/fragment/nonfile/unrepresentable URL refusal")

    for changes in [
      ["cliSchema": "wrong"], ["statusSchema": "wrong"], ["classification": "STARTING"],
      ["classification": "FOREIGN_LISTENER"], ["listenAddress": "0.0.0.0:4173"],
      ["listenAddress": "127.0.0.1:4174"], ["url": "https://example.invalid"],
      ["url": "http://127.0.0.1:4173/path"], ["url": "http://127.0.0.1:4173?x=1"],
    ] {
      let (adapter, _, view) = try fixture(response(changes))
      await open(adapter, [URL(fileURLWithPath: "/synthetic/project.mstream")])
      expect(
        view.browsers.isEmpty && view.errors == [.invalidResponse], "strict controller response")
    }
    for output in [
      response(ok: false), .init(status: 0, stdout: Data("not JSON".utf8), stderr: Data()),
      .init(status: 0, stdout: Data(repeating: 65, count: 65_537), stderr: Data()),
      .init(status: 0, stdout: response().stdout, stderr: Data(repeating: 65, count: 65_537)),
    ] {
      let (adapter, _, view) = try fixture(output)
      await open(adapter, [URL(fileURLWithPath: "/synthetic/project.mstream")])
      expect(
        view.browsers.isEmpty && view.errors == [.invalidResponse], "malformed/over-limit response")
    }
    print("PASS exact schemas/OWNED/address/URL and bounded response validation")

    for (output, fails) in [(response(status: 1), false), (response(), true)] {
      let (adapter, _, view) = try fixture(output, fails: fails)
      await open(adapter, [URL(fileURLWithPath: "/synthetic/project.mstream")])
      expect(
        view.browsers.isEmpty && view.errors == [.controller], "runner failures do not open browser"
      )
    }
    print("PASS injected process failure/nonzero exit")

    let (recovering, recoverySpy, recoveryView) = try fixture(fails: true)
    await open(recovering, [URL(fileURLWithPath: "/synthetic/failed/project.mstream")])
    await recoverySpy.succeed(response())
    await open(recovering, [URL(fileURLWithPath: "/synthetic/retry/project.mstream")])
    let recoveryCalls = await recoverySpy.calls
    expect(
      recoveryCalls.count == 2 && recoveryView.errors == [.controller]
        && recoveryView.browsers.count == 1,
      "failure releases inflight without pinning failed selection")
    print("PASS retry after failure without stale selection or inflight state")

    let (serial, serialSpy, serialView) = try fixture(held: true)
    let document = URL(fileURLWithPath: "/synthetic/serial/project.mstream")
    let task = serial.handleOpen([document])!
    expect(serial.handleOpen([document]) == nil, "inflight request rejected")
    for _ in 0..<1000 {
      if !(await serialSpy.calls.isEmpty) { break }
      await Task.yield()
    }
    let startedCalls = await serialSpy.calls
    expect(startedCalls.count == 1, "injected runner begins without an unbounded test wait")
    await serialSpy.unblock()
    await task.value
    let serialCalls = await serialSpy.calls
    expect(
      serialCalls.count == 1 && serialView.errors == [.busy] && serialView.browsers.count == 1,
      "serialized event handling")
    print("PASS inflight serialization")

    let (repeatAdapter, repeatSpy, repeatView) = try fixture()
    await open(repeatAdapter, [document])
    await open(repeatAdapter, [document])
    await open(repeatAdapter, [URL(fileURLWithPath: "/synthetic/different/project.mstream")])
    let repeatCalls = await repeatSpy.calls
    expect(
      repeatCalls.count == 2 && repeatView.browsers.count == 2
        && repeatView.errors == [.differentDocument], "repeat delegates; switching refuses")
    print("PASS repeat same document and different-document refusal")

    let (browserFailure, browserSpy, browserView) = try fixture()
    browserView.failBrowser = true
    await open(browserFailure, [document])
    await open(browserFailure, [URL(fileURLWithPath: "/synthetic/different/project.mstream")])
    let browserCalls = await browserSpy.calls
    expect(
      browserCalls.count == 1 && browserView.errors == [.browser, .differentDocument],
      "successful controller pins document despite browser failure")
    print("PASS browser failure retains successful document selection")
    print("9 adapter proof groups passed; no native app/process/browser/runtime/network execution")
  }
}

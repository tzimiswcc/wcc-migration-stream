import AppKit
import Darwin
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
      trustedRuntimeRoot: URL(
        fileURLWithPath: "/trusted runtime/πελάτης%literal", isDirectory: true),
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

  nonisolated private static let syntheticNativeDocument =
    "/synthetic-production/customer readiness/πελάτης%20literal/$(touch nothing); 'quoted'/project.mstream"

  /// Only the private copy named as the controller can enter helper mode.
  /// Invalid arguments exit rather than recursively running the test harness.
  nonisolated private static func syntheticHelper() -> Bool {
    let arguments = CommandLine.arguments
    guard URL(fileURLWithPath: arguments[0]).lastPathComponent == "migration-stream-local-runtime"
    else { return false }
    Darwin.signal(SIGALRM, SIG_DFL)
    Darwin.alarm(50)  // Every helper self-expires even if the parent runner fails.
    guard arguments.count == 4, arguments[1] == "open", arguments[2] == "--",
      getenv("MIGRATION_STREAM_ADAPTER_TEST_SENTINEL") == nil
    else { Darwin._exit(64) }
    let document = arguments[3]
    switch document {
    case syntheticNativeDocument:
      let exactNative = URL(fileURLWithPath: syntheticNativeDocument)
        .withUnsafeFileSystemRepresentation { pointer -> Data? in
          guard let pointer else { return nil }
          return Data(bytes: pointer, count: strlen(pointer))
        }
      guard exactNative == Data(document.utf8) else { Darwin._exit(65) }
      FileHandle.standardOutput.write(response().stdout)
    case "/synthetic-production/nonzero/project.mstream":
      Darwin._exit(7)
    case "/synthetic-production/malformed/project.mstream":
      FileHandle.standardOutput.write(Data("not JSON".utf8))
    case "/synthetic-production/stdout-cap/project.mstream":
      FileHandle.standardOutput.write(Data(repeating: 65, count: 262_144))
    case "/synthetic-production/stderr-cap/project.mstream":
      FileHandle.standardError.write(Data(repeating: 66, count: 262_144))
    case "/synthetic-production/stderr-pressure/project.mstream":
      FileHandle.standardError.write(Data(repeating: 67, count: 49_152))
      FileHandle.standardOutput.write(response().stdout)
    case "/synthetic-production/deadline/project.mstream":
      Darwin.signal(SIGTERM, SIG_IGN)  // Exercise the runner's bounded kill fallback.
      Thread.sleep(forTimeInterval: 50)
    default:
      Darwin._exit(64)
    }
    Darwin._exit(0)
  }

  @MainActor private static func productionRunnerProof() async throws {
    let manager = FileManager.default
    let fixture = manager.temporaryDirectory.appendingPathComponent(
      "migration-stream-adapter-helper-\(UUID().uuidString)", isDirectory: true)
    let runtime = fixture.appendingPathComponent(
      "synthetic runtime πελάτης%literal", isDirectory: true)
    let libexec = runtime.appendingPathComponent("libexec", isDirectory: true)
    try manager.createDirectory(at: libexec, withIntermediateDirectories: true)
    defer { try? manager.removeItem(at: fixture) }
    let helper = libexec.appendingPathComponent("migration-stream-local-runtime")
    try manager.copyItem(at: URL(fileURLWithPath: CommandLine.arguments[0]), to: helper)
    try manager.setAttributes([.posixPermissions: 0o700], ofItemAtPath: helper.path)
    let sentinel = "MIGRATION_STREAM_ADAPTER_TEST_SENTINEL"
    let previous = getenv(sentinel).map { String(cString: $0) }
    setenv(sentinel, "synthetic-parent-only", 1)
    defer {
      if let previous { setenv(sentinel, previous, 1) } else { unsetenv(sentinel) }
    }
    let cases: [(String, DocumentOpenAdapter.Failure?)] = [
      (syntheticNativeDocument, nil),
      ("/synthetic-production/nonzero/project.mstream", .controller),
      ("/synthetic-production/malformed/project.mstream", .invalidResponse),
      ("/synthetic-production/stdout-cap/project.mstream", .controller),
      ("/synthetic-production/stderr-cap/project.mstream", .controller),
      ("/synthetic-production/stderr-pressure/project.mstream", nil),
      ("/synthetic-production/deadline/project.mstream", .controller),
    ]
    for (document, failure) in cases {
      let view = Presentation()
      // Omit runner: exercise the unchanged default Foundation Process boundary.
      let adapter = try DocumentOpenAdapter(
        trustedRuntimeRoot: runtime,
        browser: { view.browsers.append($0) },
        errorPresenter: { view.errors.append($0) }
      )
      let started = DispatchTime.now().uptimeNanoseconds
      await open(adapter, [URL(fileURLWithPath: document)])
      let elapsed = Double(DispatchTime.now().uptimeNanoseconds - started) / 1_000_000_000
      if let failure {
        expect(view.errors == [failure] && view.browsers.isEmpty, "production refusal: \(document)")
      } else {
        expect(
          view.errors.isEmpty && view.browsers.map(\.absoluteString) == ["http://127.0.0.1:4173"],
          "production success: \(document)")
      }
      if document.contains("/deadline/") {
        expect(elapsed >= 44 && elapsed <= 51, "real45s deadline plus bounded cleanup")
        print(
          "PASS production45s deadline/TERM-ignore cleanup elapsed=\(elapsed)s (helper self-expires50s)"
        )
      } else {
        expect(elapsed < 10, "bounded process/pipe completion without deadlock")
        print(
          "PASS production helper \(URL(fileURLWithPath: document).deletingLastPathComponent().lastPathComponent)"
        )
      }
    }
    try manager.removeItem(at: fixture)
    expect(!manager.fileExists(atPath: fixture.path), "private synthetic fixture cleanup")
    print(
      "7 production Foundation Process proof cases passed; only private self-expiring synthetic helpers ran"
    )
  }

  @MainActor static func main() async throws {
    if syntheticHelper() { return }
    Darwin.signal(SIGALRM, SIG_DFL)
    Darwin.alarm(70)  // Bound the whole source-proof executable, not just each helper.
    defer { Darwin.alarm(0) }
    // Never construct NSApplication, invoke NSWorkspace, register documents,
    // launch a product controller/runtime, or touch an actual workspace.
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
    print("9 injected handler proof groups passed; no native app/browser/runtime/network execution")
    try await productionRunnerProof()
  }
}

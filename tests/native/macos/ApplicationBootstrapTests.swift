import AppKit
import Foundation

private actor BootstrapRunnerSpy {
  struct Call: Sendable {
    let executable: URL
    let arguments: [String]
  }
  var calls: [Call] = []
  var output: DocumentOpenAdapter.Output
  var held = false
  var continuation: CheckedContinuation<Void, Never>?
  init(_ output: DocumentOpenAdapter.Output) { self.output = output }
  func run(_ executable: URL, _ arguments: [String]) async -> DocumentOpenAdapter.Output {
    calls.append(Call(executable: executable, arguments: arguments))
    if held { await withCheckedContinuation { continuation = $0 } }
    return output
  }
  func hold() { held = true }
  func release() {
    held = false
    continuation?.resume()
    continuation = nil
  }
}

@MainActor private final class BootstrapView {
  var roots: [URL] = []
  var browsers: [URL] = []
  var errors: [String] = []
  func report(_ error: Error) {
    if let error = error as? OpenObjectCoordinator.Failure {
      errors.append("coordinator:\(error)")
    } else if let error = error as? DocumentOpenAdapter.Failure {
      errors.append("adapter:\(error)")
    } else {
      errors.append("unexpected")
    }
  }
}

@MainActor private final class BootstrapPickerSpy {
  enum Failure: Error { case syntheticPresentation }
  var configurations: [OpenObjectCoordinator.PickerConfiguration] = []
  var results: [[URL]?] = []
  var fails = false
  var held = false
  var continuation: CheckedContinuation<[URL]?, Error>?
  func pick(_ configuration: OpenObjectCoordinator.PickerConfiguration) async throws -> [URL]? {
    configurations.append(configuration)
    if held { return try await withCheckedThrowingContinuation { continuation = $0 } }
    if fails { throw Failure.syntheticPresentation }
    return results.isEmpty ? nil : results.removeFirst()
  }
  func finish(_ result: [URL]?) {
    held = false
    continuation?.resume(returning: result)
    continuation = nil
  }
}

@main struct ApplicationBootstrapTests {
  @MainActor private static func expect(_ condition: @autoclosure () -> Bool, _ message: String) {
    precondition(condition(), message)
  }
  private static func response(_ classification: String = "OWNED") -> DocumentOpenAdapter.Output {
    let value: [String: Any] = [
      "schemaVersion": "migration-stream/assisted-local-runtime/cli/v1", "ok": true,
      "result": [
        "schemaVersion": "migration-stream/assisted-local-runtime/status/v2",
        "classification": classification, "listenAddress": "127.0.0.1:4173",
        "url": "http://127.0.0.1:4173",
      ],
    ]
    return .init(
      status: 0, stdout: try! JSONSerialization.data(withJSONObject: value), stderr: Data())
  }
  private static func native(_ url: URL) -> Data {
    url.withUnsafeFileSystemRepresentation { Data(bytes: $0!, count: strlen($0!)) }
  }
  @MainActor private static func coordinator(
    _ resources: URL?, _ picker: BootstrapPickerSpy, _ runner: BootstrapRunnerSpy,
    _ view: BootstrapView
  ) throws -> OpenObjectCoordinator {
    try OpenObjectCoordinator(
      resourceURL: resources, picker: { try await picker.pick($0) },
      adapterFactory: { root, report in
        view.roots.append(root)
        return try DocumentOpenAdapter(
          trustedRuntimeRoot: root,
          runner: { executable, arguments in await runner.run(executable, arguments) },
          browser: { view.browsers.append($0) }, errorPresenter: report)
      }, errorPresenter: { view.report($0) })
  }
  @MainActor private static func menu(_ coordinator: OpenObjectCoordinator) async {
    if let operation = coordinator.openObject() { await operation.value }
  }

  @MainActor private static func platformBundleResourceProjection() async throws {
    let files = FileManager.default
    let fixture = files.temporaryDirectory.appendingPathComponent(
      "migration-stream-bundle-resource-\(UUID().uuidString)", isDirectory: true
    ).resolvingSymlinksInPath()
    let bundleRoot = fixture.appendingPathComponent(
      "synthetic πελάτης% literal.bundle", isDirectory: true)
    let resources = bundleRoot.appendingPathComponent("Contents/Resources", isDirectory: true)
    try files.createDirectory(
      at: resources, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
    defer { try? files.removeItem(at: fixture) }
    let info: [String: Any] = [
      "CFBundleIdentifier": "test.synthetic.resource-projection",
      "CFBundleName": "Synthetic resource projection", "CFBundlePackageType": "BNDL",
      "CFBundleVersion": "1", "CFBundleExecutable": "Synthetic",
    ]
    try PropertyListSerialization.data(fromPropertyList: info, format: .xml, options: 0)
      .write(to: bundleRoot.appendingPathComponent("Contents/Info.plist"))
    guard let bundle = Bundle(url: bundleRoot), let raw = bundle.resourceURL else {
      preconditionFailure("Foundation must expose the synthetic bundle resource URL")
    }
    expect(raw.baseURL != nil, "fixture reproduces platform base-backed resource metadata")
    let picker = BootstrapPickerSpy()
    let runner = BootstrapRunnerSpy(response())
    let rejected = BootstrapView()
    do {
      _ = try coordinator(raw, picker, runner, rejected)
      preconditionFailure("strict coordinator must continue refusing directly supplied based URLs")
    } catch {
      expect(
        error as? OpenObjectCoordinator.Failure == .invalidResources,
        "raw platform URL reproduces the explicit resource admission failure")
    }
    let projected = ApplicationBootstrap.resourceURL(in: bundle)
    expect(
      projected?.baseURL == nil && projected.map(native) == native(raw)
        && projected.map(native) == native(resources),
      "production Bundle projection resolves its own base without changing native resource bytes")
    let view = BootstrapView()
    let selected = try coordinator(projected, picker, runner, view)
    let runtime = resources.appendingPathComponent("Runtime", isDirectory: true)
    expect(
      view.roots.count == 1 && native(view.roots[0]) == native(runtime),
      "actual coordinator/adapter construction receives the expected native Resources/Runtime")
    expect(
      native(view.roots[0].appendingPathComponent("libexec/migration-stream-local-runtime"))
        == native(
          resources.appendingPathComponent("Runtime/libexec/migration-stream-local-runtime")),
      "fixed controller derives beneath the exact platform Runtime root")
    let constructedCalls = await runner.calls
    expect(
      constructedCalls.isEmpty && picker.configurations.isEmpty && view.browsers.isEmpty
        && view.errors.isEmpty && rejected.roots.isEmpty,
      "projection and real constructors perform no picker, runner or browser operation")
    for invalidDocument in [
      URL(string: "project.mstream")!, URL(string: "project.mstream", relativeTo: resources)!,
    ] {
      expect(
        selected.forwardDocuments([invalidDocument]) == nil,
        "platform-resource normalization must never admit a relative/based document")
    }
    let refusedCalls = await runner.calls
    expect(
      refusedCalls.isEmpty && view.browsers.isEmpty && picker.configurations.isEmpty
        && view.errors == ["adapter:invalidDocument", "adapter:invalidDocument"],
      "unchanged document URL guards refuse before runner/browser/picker")
    print("PASS actual Bundle resource projection, constructor and strict based-document refusal")
  }

  @MainActor static func main() async throws {
    // Only pure Bundle metadata projection and injected constructors/configuration.
    // Never call make/configure, NSApplication, NSMenu or NSOpenPanel.
    try await platformBundleResourceProjection()
    let resources = URL(
      fileURLWithPath: "/synthetic app/πελάτης%literal/Contents/Resources", isDirectory: true)
    let document = URL(
      fileURLWithPath:
        "/synthetic/customer readiness/πελάτης%20literal/$(literal); 'quoted'/project.mstream")
    let picker = BootstrapPickerSpy()
    let view = BootstrapView()
    let runner = BootstrapRunnerSpy(response())
    picker.results = [[document]]
    let selected = try coordinator(resources, picker, runner, view)
    await menu(selected)
    let calls = await runner.calls
    let runtime = resources.appendingPathComponent("Runtime", isDirectory: true)
    expect(
      view.roots.count == 1 && native(view.roots[0]) == native(runtime),
      "exact Resources/Runtime, one retained adapter")
    expect(
      calls.count == 1
        && native(calls[0].executable)
          == native(runtime.appendingPathComponent("libexec/migration-stream-local-runtime")),
      "fixed controller beneath exact resource root")
    expect(
      calls[0].arguments.count == 3 && calls[0].arguments[0] == "open"
        && calls[0].arguments[1] == "--" && Data(calls[0].arguments[2].utf8) == native(document),
      "lossless original document selection")
    expect(
      view.browsers.count == 1 && view.errors.isEmpty, "menu routes through approved response guard"
    )
    let configuration = picker.configurations[0]
    expect(
      configuration.canChooseFiles && !configuration.canChooseDirectories
        && !configuration.allowsMultipleSelection && !configuration.resolvesAliases,
      "files-only one selection without alias resolution")
    let menuConfiguration = OpenObjectCoordinator.menuConfiguration
    expect(
      menuConfiguration.menuTitle == "File" && menuConfiguration.title == "Open Object…"
        && menuConfiguration.keyEquivalent == "o" && menuConfiguration.modifiers == .command,
      "source File/Open Object CmdO configuration")
    print("PASS resource directory/native path, picker and menu configuration")

    for invalid in [
      nil, URL(string: "https://example.invalid/Resources"),
      URL(string: "file://remote/Resources/"),
      URL(string: "file:///Resources/?query=1"), URL(string: "file:///Resources/#fragment"),
      URL(string: "file:///Resources/%FF/"), URL(string: "relative/Resources/"),
    ] as [URL?] {
      let invalidView = BootstrapView()
      let invalidPicker = BootstrapPickerSpy()
      do {
        _ = try coordinator(invalid, invalidPicker, runner, invalidView)
        preconditionFailure("missing/nonlocal/unrepresentable resources must not fall back")
      } catch {
        expect(error is OpenObjectCoordinator.Failure, "resource failure is explicit")
      }
      expect(
        invalidView.roots.isEmpty && invalidPicker.configurations.isEmpty
          && invalidView.browsers.isEmpty, "resource failure before adapter or picker; no fallback")
    }
    print("PASS missing/invalid resources fail before factory; no root fallback")

    let cancelPicker = BootstrapPickerSpy()
    let cancelView = BootstrapView()
    let cancelRunner = BootstrapRunnerSpy(response())
    cancelPicker.results = [nil, [], nil]
    let cancellation = try coordinator(resources, cancelPicker, cancelRunner, cancelView)
    await menu(cancellation)
    await menu(cancellation)
    await menu(cancellation)
    let cancelledCalls = await cancelRunner.calls
    expect(
      cancelPicker.configurations.count == 3 && cancelledCalls.isEmpty && cancelView.errors.isEmpty,
      "cancel/empty selection no operation and state resets")
    cancelPicker.fails = true
    await menu(cancellation)
    cancelPicker.fails = false
    await menu(cancellation)
    expect(
      cancelPicker.configurations.count == 5
        && cancelView.errors == ["coordinator:pickerPresentation"],
      "presentation failure resets picker with explicit error")
    print("PASS cancellation/empty/presentation-error reset")

    let heldPicker = BootstrapPickerSpy()
    let heldView = BootstrapView()
    heldPicker.held = true
    let held = try coordinator(resources, heldPicker, cancelRunner, heldView)
    let heldOperation = held.openObject()!
    expect(
      held.openObject() == nil && heldView.errors == ["coordinator:pickerBusy"],
      "only one picker in flight")
    for _ in 0..<1000 {
      if heldPicker.continuation != nil { break }
      await Task.yield()
    }
    expect(
      heldPicker.continuation != nil && heldPicker.configurations.count == 1,
      "one injected picker starts")
    heldPicker.finish(nil)
    await heldOperation.value
    await menu(held)
    expect(heldPicker.configurations.count == 2, "picker reset after cancellation")
    print("PASS one picker in flight and reset")

    if let operation = selected.forwardDocuments([document]) { await operation.value }
    picker.results = [[URL(fileURLWithPath: "/synthetic/different/project.mstream")]]
    await menu(selected)
    let repeated = await runner.calls
    expect(
      view.roots.count == 1 && repeated.count == 2 && view.browsers.count == 2
        && view.errors == ["adapter:differentDocument"],
      "menu/native forwarding share one adapter; same repeats, different refuses")
    print("PASS native/menu route retention and repeat/different-document guard")

    let invalidPicker = BootstrapPickerSpy()
    let invalidView = BootstrapView()
    let invalidRunner = BootstrapRunnerSpy(response())
    invalidPicker.results = [
      [document, URL(fileURLWithPath: "/synthetic/second/project.mstream")],
      [URL(string: "file:///synthetic/project.mstream?query=unchanged")!],
    ]
    let invalidSelection = try coordinator(resources, invalidPicker, invalidRunner, invalidView)
    await menu(invalidSelection)
    await menu(invalidSelection)
    let invalidCalls = await invalidRunner.calls
    expect(
      invalidCalls.isEmpty
        && invalidView.errors == ["adapter:invalidDocument", "adapter:invalidDocument"],
      "cardinality and raw URL query reach adapter unchanged")
    print("PASS unchanged selected URL/cardinality refusal")

    let responsePicker = BootstrapPickerSpy()
    let responseView = BootstrapView()
    let responseRunner = BootstrapRunnerSpy(response("FOREIGN_LISTENER"))
    responsePicker.results = [[document]]
    let guarded = try coordinator(resources, responsePicker, responseRunner, responseView)
    await menu(guarded)
    if let operation = guarded.forwardDocuments([document]) { await operation.value }
    expect(
      responseView.browsers.isEmpty
        && responseView.errors == ["adapter:invalidResponse", "adapter:invalidResponse"],
      "menu/native responses remain guarded by approved adapter")
    print("PASS existing invalid response guard on both routes")

    let overlapPicker = BootstrapPickerSpy()
    let overlapView = BootstrapView()
    let overlapRunner = BootstrapRunnerSpy(response())
    await overlapRunner.hold()
    overlapPicker.results = [[document]]
    let overlap = try coordinator(resources, overlapPicker, overlapRunner, overlapView)
    let first = overlap.openObject()!
    for _ in 0..<1000 {
      if !(await overlapRunner.calls.isEmpty) { break }
      await Task.yield()
    }
    let began = await overlapRunner.calls
    expect(began.count == 1, "selected document starts injected runner")
    overlapPicker.held = true
    let second = overlap.openObject()!
    for _ in 0..<1000 {
      if overlapPicker.continuation != nil { break }
      await Task.yield()
    }
    expect(overlapPicker.continuation != nil, "new picker permitted after prior selection")
    await overlapRunner.release()
    await first.value
    expect(
      overlap.openObject() == nil && overlapView.errors == ["coordinator:pickerBusy"],
      "older controller completion cannot clear newer picker state")
    overlapPicker.finish(nil)
    await second.value
    print("PASS picker state isolated from older controller completion")
    print(
      "9 coordinator proof groups passed; no UI/app/process/browser/nativeevent/network/runtime execution"
    )
  }
}

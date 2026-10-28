import AppKit
import Darwin
import Foundation

/// Source-only delegate adapter. The caller must supply its trusted intact runtime subtree.
/// This does not create an application, register documents, or choose an installed runtime.
@MainActor
final class DocumentOpenAdapter: NSObject, NSApplicationDelegate {
  struct Output: Sendable {
    let status: Int32
    let stdout: Data
    let stderr: Data
  }

  enum Failure: Error, Equatable {
    case invalidDocument, busy, differentDocument, controller, invalidResponse, browser
  }

  typealias Runner = @Sendable (URL, [String]) async throws -> Output
  private let executable: URL
  private let run: Runner
  private let present: @MainActor (URL) throws -> Void
  private let report: @MainActor (Failure) -> Void
  private var inFlight = false
  private var selectedPath: Data?
  private static let outputLimit = 65_536
  private static let address = "127.0.0.1:4173"
  private static let loopback = "http://127.0.0.1:4173"

  init(
    trustedRuntimeRoot: URL,
    runner: @escaping Runner = DocumentOpenAdapter.runController,
    browser: @escaping @MainActor (URL) throws -> Void = { url in
      guard NSWorkspace.shared.open(url) else { throw Failure.browser }
    },
    errorPresenter: @escaping @MainActor (Failure) -> Void
  ) throws {
    let root = try Self.nativePath(trustedRuntimeRoot, allowDirectorySeparator: true)
    executable = URL(fileURLWithPath: root, isDirectory: true)
      .appendingPathComponent("libexec", isDirectory: true)
      .appendingPathComponent("migration-stream-local-runtime", isDirectory: false)
    run = runner
    present = browser
    report = errorPresenter
    super.init()
  }

  func application(_ application: NSApplication, open urls: [URL]) {
    handleOpen(urls)
  }

  /// The real event handler, also callable without starting NSApplication in source tests.
  @discardableResult
  func handleOpen(_ urls: [URL]) -> Task<Void, Never>? {
    guard !inFlight else {
      report(.busy)
      return nil
    }
    guard urls.count == 1, let path = try? Self.nativePath(urls[0]) else {
      report(.invalidDocument)
      return nil
    }
    let identity = Data(path.utf8)
    guard selectedPath == nil || selectedPath == identity else {
      report(.differentDocument)
      return nil
    }
    inFlight = true
    return Task { @MainActor in
      defer { inFlight = false }
      do {
        let output = try await run(executable, ["open", "--", path])
        guard output.status == 0 else { throw Failure.controller }
        let destination = try Self.validatedDestination(output)
        // Pin after a validated successful start even if browser presentation fails.
        selectedPath = identity
        do { try present(destination) } catch { throw Failure.browser }
      } catch {
        report(error as? Failure ?? .controller)
      }
    }
  }

  private static func nativePath(_ url: URL, allowDirectorySeparator: Bool = false) throws -> String
  {
    guard url.baseURL == nil, url.isFileURL,
      let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
      components.host == nil || components.host == "",
      components.user == nil, components.password == nil, components.port == nil,
      components.query == nil, components.fragment == nil,
      let path = components.percentEncodedPath.removingPercentEncoding,
      path.hasPrefix("/"), !path.utf8.contains(0)
    else { throw Failure.invalidDocument }
    // Foundation omits a directory URL's terminal marker from its native form.
    // Permit that marker only on the trusted runtime root; never rewrite documents.
    var nativePath = path
    if allowDirectorySeparator && url.hasDirectoryPath && path != "/" && path.hasSuffix("/") {
      nativePath.removeLast()
    }
    let native = url.withUnsafeFileSystemRepresentation { pointer -> Data? in
      guard let pointer else { return nil }
      return Data(bytes: pointer, count: strlen(pointer))
    }
    guard native == Data(nativePath.utf8) else { throw Failure.invalidDocument }
    return nativePath
  }

  private struct Envelope: Decodable {
    let schemaVersion: String
    let ok: Bool
    let result: Status
  }
  private struct Status: Decodable {
    let schemaVersion: String
    let classification: String
    let listenAddress: String
    let url: String
  }

  private static func validatedDestination(_ output: Output) throws -> URL {
    guard output.stdout.count <= outputLimit, output.stderr.count <= outputLimit,
      let envelope = try? JSONDecoder().decode(Envelope.self, from: output.stdout),
      envelope.schemaVersion == "migration-stream/assisted-local-runtime/cli/v1",
      envelope.ok,
      envelope.result.schemaVersion == "migration-stream/assisted-local-runtime/status/v2",
      envelope.result.classification == "OWNED",
      envelope.result.listenAddress == address, envelope.result.url == loopback,
      let destination = URL(string: loopback)
    else { throw Failure.invalidResponse }
    return destination
  }

  /// Runs off the main actor. 45s covers the existing 5s command lock + 30s
  /// startup deadline with 10s margin. Cleanup has a separate bounded 4s wait.
  nonisolated private static func runController(_ executable: URL, _ arguments: [String])
    async throws -> Output
  {
    try await Task.detached { try runBlocking(executable, arguments) }.value
  }

  nonisolated private static func runBlocking(_ executable: URL, _ arguments: [String]) throws
    -> Output
  {
    let process = Process()
    process.executableURL = executable
    process.arguments = arguments
    process.environment = [:]
    process.standardInput = FileHandle.nullDevice
    let stdout = Pipe()
    let stderr = Pipe()
    process.standardOutput = stdout
    process.standardError = stderr
    let capture = Capture()
    let terminated = DispatchSemaphore(value: 0)
    process.terminationHandler = { _ in terminated.signal() }
    var completed = false
    defer {
      if !completed && process.isRunning {
        process.terminate()
        if terminated.wait(timeout: .now() + 2) != .success {
          // Signal only the directly owned child, never a runtime/session PID.
          if process.isRunning { Darwin.kill(process.processIdentifier, SIGKILL) }
          _ = terminated.wait(timeout: .now() + 2)
        }
      }
      for pipe in [stdout, stderr] {
        try? pipe.fileHandleForReading.close()
        try? pipe.fileHandleForWriting.close()
      }
    }
    for pipe in [stdout, stderr] {
      let descriptor = pipe.fileHandleForReading.fileDescriptor
      let flags = fcntl(descriptor, F_GETFL)
      guard flags >= 0, fcntl(descriptor, F_SETFL, flags | O_NONBLOCK) >= 0
      else { throw Failure.controller }
    }
    try process.run()
    let deadline = DispatchTime.now() + 45
    while DispatchTime.now().uptimeNanoseconds < deadline.uptimeNanoseconds {
      try capture.drain(stdout.fileHandleForReading, stdout: true)
      try capture.drain(stderr.fileHandleForReading, stdout: false)
      if terminated.wait(timeout: .now() + 0.01) == .success {
        completed = true
        break
      }
    }
    guard completed else { throw Failure.controller }
    // Drain currently available bytes after exit, never wait for inherited EOF.
    try capture.drain(stdout.fileHandleForReading, stdout: true)
    try capture.drain(stderr.fileHandleForReading, stdout: false)
    return Output(status: process.terminationStatus, stdout: capture.output, stderr: capture.error)
  }
}

/// Nonblocking reads stay on the one background runner; each drain is bounded.
private final class Capture {
  private(set) var output = Data(), error = Data()
  func drain(_ handle: FileHandle, stdout: Bool) throws {
    var buffer = [UInt8](repeating: 0, count: 4096)
    for _ in 0..<18 {
      let count = Darwin.read(handle.fileDescriptor, &buffer, buffer.count)
      if count == 0 { return }
      if count < 0 {
        if errno == EAGAIN || errno == EWOULDBLOCK { return }
        if errno == EINTR { continue }
        throw DocumentOpenAdapter.Failure.controller
      }
      let existing = stdout ? output.count : error.count
      guard count <= 65_536 - existing else { throw DocumentOpenAdapter.Failure.controller }
      if stdout {
        output.append(contentsOf: buffer.prefix(count))
      } else {
        error.append(contentsOf: buffer.prefix(count))
      }
    }
    throw DocumentOpenAdapter.Failure.controller
  }
}

import AppKit
import Foundation

/// Own this coordinator for the application's lifetime. It owns exactly one adapter.
@MainActor
final class OpenObjectCoordinator: NSObject, NSApplicationDelegate {
  struct PickerConfiguration: Equatable {
    let canChooseFiles = true
    let canChooseDirectories = false
    let allowsMultipleSelection = false
    let resolvesAliases = false
  }
  struct MenuConfiguration: Equatable {
    let menuTitle = "File"
    let title = "Open Object…"
    let keyEquivalent = "o"
    let modifiers: NSEvent.ModifierFlags = .command
  }
  enum Failure: Error, Equatable {
    case missingResources, invalidResources, pickerBusy, pickerPresentation
  }
  typealias ErrorPresenter = @MainActor (Error) -> Void
  typealias Picker = @MainActor (PickerConfiguration) async throws -> [URL]?
  typealias AdapterFactory =
    @MainActor (
      URL, @escaping @MainActor (DocumentOpenAdapter.Failure) -> Void
    ) throws -> DocumentOpenAdapter

  static let pickerConfiguration = PickerConfiguration()
  static let menuConfiguration = MenuConfiguration()
  private let adapter: DocumentOpenAdapter
  private let picker: Picker
  private let report: ErrorPresenter
  private var pickerInFlight = false

  init(
    resourceURL: URL?,
    picker: @escaping Picker = OpenObjectCoordinator.presentPicker,
    adapterFactory: AdapterFactory = { root, report in
      try DocumentOpenAdapter(trustedRuntimeRoot: root, errorPresenter: report)
    },
    errorPresenter: @escaping ErrorPresenter
  ) throws {
    let runtime = try Self.runtimeRoot(resourceURL)
    adapter = try adapterFactory(runtime) { errorPresenter($0) }
    self.picker = picker
    report = errorPresenter
    super.init()
  }

  /// Both the native delegate and menu selection use this same retained adapter.
  @discardableResult
  func forwardDocuments(_ urls: [URL]) -> Task<Void, Never>? {
    adapter.handleOpen(urls)
  }

  func application(_ application: NSApplication, open urls: [URL]) {
    forwardDocuments(urls)
  }

  @objc func chooseObject(_ sender: Any?) {
    openObject()
  }

  @discardableResult
  func openObject() -> Task<Void, Never>? {
    guard !pickerInFlight else {
      report(Failure.pickerBusy)
      return nil
    }
    pickerInFlight = true
    return Task { @MainActor in
      let selected: [URL]?
      do {
        selected = try await picker(Self.pickerConfiguration)
      } catch {
        pickerInFlight = false
        report(Failure.pickerPresentation)
        return
      }
      // Release picker state before forwarding; an older controller request must
      // never clear the state of a newer panel when it eventually completes.
      pickerInFlight = false
      guard let selected, !selected.isEmpty else { return }
      if let operation = forwardDocuments(selected) { await operation.value }
    }
  }

  private static func runtimeRoot(_ resourceURL: URL?) throws -> URL {
    guard let url = resourceURL else { throw Failure.missingResources }
    guard url.baseURL == nil, url.isFileURL,
      let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
      components.host == nil || components.host == "",
      components.user == nil, components.password == nil, components.port == nil,
      components.query == nil, components.fragment == nil,
      var path = components.percentEncodedPath.removingPercentEncoding,
      path.hasPrefix("/"), !path.utf8.contains(0)
    else { throw Failure.invalidResources }
    if url.hasDirectoryPath && path != "/" && path.hasSuffix("/") { path.removeLast() }
    let native = url.withUnsafeFileSystemRepresentation { pointer -> Data? in
      guard let pointer else { return nil }
      return Data(bytes: pointer, count: strlen(pointer))
    }
    guard native == Data(path.utf8) else { throw Failure.invalidResources }
    return URL(fileURLWithPath: path, isDirectory: true)
      .appendingPathComponent("Runtime", isDirectory: true)
  }

  private static func presentPicker(_ configuration: PickerConfiguration) async throws -> [URL]? {
    await withCheckedContinuation { continuation in
      let panel = NSOpenPanel()
      panel.canChooseFiles = configuration.canChooseFiles
      panel.canChooseDirectories = configuration.canChooseDirectories
      panel.allowsMultipleSelection = configuration.allowsMultipleSelection
      panel.resolvesAliases = configuration.resolvesAliases
      panel.begin { response in
        continuation.resume(returning: response == .OK ? panel.urls : nil)
      }
    }
  }
}

/// Explicit, uninvoked composition for a future wrapper; no @main or automatic startup.
/// The wrapper must retain this owner. No environment/current-directory/path fallback exists.
@MainActor
final class ApplicationBootstrap {
  let coordinator: OpenObjectCoordinator
  private init(_ coordinator: OpenObjectCoordinator) { self.coordinator = coordinator }

  /// Resolve only platform Bundle metadata; callers still validate the absolute file URL.
  static func resourceURL(in bundle: Bundle) -> URL? {
    bundle.resourceURL?.absoluteURL
  }

  static func make(errorPresenter: @escaping OpenObjectCoordinator.ErrorPresenter) throws
    -> ApplicationBootstrap
  {
    let coordinator = try OpenObjectCoordinator(
      resourceURL: resourceURL(in: .main), errorPresenter: errorPresenter)
    return ApplicationBootstrap(coordinator)
  }

  /// Call only from a separately reviewed real application wrapper.
  func configure(_ application: NSApplication) {
    let configuration = OpenObjectCoordinator.menuConfiguration
    let main = NSMenu()
    let item = NSMenuItem(title: configuration.menuTitle, action: nil, keyEquivalent: "")
    let file = NSMenu(title: configuration.menuTitle)
    let open = NSMenuItem(
      title: configuration.title, action: #selector(OpenObjectCoordinator.chooseObject(_:)),
      keyEquivalent: configuration.keyEquivalent)
    open.keyEquivalentModifierMask = configuration.modifiers
    open.target = coordinator
    file.addItem(open)
    item.submenu = file
    main.addItem(item)
    application.mainMenu = main
    application.delegate = coordinator
  }
}

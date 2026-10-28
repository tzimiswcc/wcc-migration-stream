import AppKit
import Darwin

/// Source entry point only; application startup and UI remain separately unqualified.
@main
struct MigrationStreamApplication {
  @MainActor static func main() {
    let application = NSApplication.shared
    do {
      let bootstrap = try ApplicationBootstrap.make(errorPresenter: presentError)
      bootstrap.configure(application)
      withExtendedLifetime(bootstrap) { application.run() }
    } catch {
      presentError(error)
      // Resource/bootstrap failure must never enter the application run loop.
      Darwin.exit(EXIT_FAILURE)
    }
  }

  @MainActor private static func presentError(_ error: Error) {
    let message: String
    switch error {
    case DocumentOpenAdapter.Failure.invalidDocument:
      message = "Choose one final bound project.mstream file from an intact object folder."
    case DocumentOpenAdapter.Failure.busy:
      message = "An object request is already in progress. Wait for it to finish and try again."
    case DocumentOpenAdapter.Failure.differentDocument:
      message = "Another object is already selected. This application does not switch objects."
    case DocumentOpenAdapter.Failure.invalidResponse:
      message =
        "The local runtime did not return a verified local address. Preserve the object and inspect its runtime status."
    case DocumentOpenAdapter.Failure.browser:
      message =
        "The local runtime responded successfully, but its browser page could not be opened."
    case OpenObjectCoordinator.Failure.pickerBusy:
      message = "An Open Object panel is already active. Complete or cancel that selection first."
    case OpenObjectCoordinator.Failure.pickerPresentation:
      message = "The object picker could not be presented. Try Open Object again."
    case OpenObjectCoordinator.Failure.missingResources,
      OpenObjectCoordinator.Failure.invalidResources:
      message =
        "Application resources are missing or invalid. Use an intact engineering application."
    default:
      message =
        "The local request could not complete. Preserve the object and inspect its runtime status before retrying."
    }
    let alert = NSAlert()
    alert.alertStyle = .warning
    alert.messageText = "Migration Stream could not complete the request"
    alert.informativeText = message
    alert.addButton(withTitle: "OK")
    alert.runModal()
  }
}

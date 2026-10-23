import AppKit
import SwiftUI
import UserNotifications

@main
struct MachApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate

    var body: some Scene {
        Window("Mach", id: "main") {
            switch delegate.boot {
            case .success(let model): ContentView(model: model)
            case .failure(let error):
                ContentUnavailableView("Mach couldn't start", systemImage: "exclamationmark.triangle", description: Text(String(describing: error)))
                    .frame(minWidth: 600, minHeight: 400)
            }
        }
        .commands { MenuCommands(model: try? delegate.boot.get()) }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    let boot: Result<AppModel, Error> = Result { AppModel(engine: try Engine()) }
    private var debugServer: DebugServer?
    private var keyMonitor: Any?

    // The delegate must be set before launch finishes, or a click that
    // launched the app is never delivered.
    func applicationWillFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = self
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        guard case .success(let model) = boot else { return }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [model] event in
            guard let chord = KeyChord.chord(for: event) else { return event }
            if let consumed = model.handleRouteKey(chord) { return consumed ? nil : event }
            return Self.shouldIntercept(event) && model.handleKey(chord) ? nil : event
        }
        let port = ProcessInfo.processInfo.environment["MACH_DEBUG_PORT"].flatMap(UInt16.init) ?? 7366
        debugServer = DebugServer(port: port, model: model)
        Task { await model.listen() }
        Task {
            await model.load()
            do {
                model.notificationsAllowed = try await UNUserNotificationCenter.current()
                    .requestAuthorization(options: [.alert, .sound, .badge])
            } catch {
                model.notificationsAllowed = false
                model.engine.log("warn", "notification authorization: \(error)")
            }
        }
    }

    /// Text fields keep their keys; Esc still reaches the keymap.
    private static func shouldIntercept(_ event: NSEvent) -> Bool {
        guard let editor = event.window?.firstResponder as? NSTextView, editor.isEditable else { return true }
        return event.keyCode == 53
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse
    ) async {
        guard let threadId = response.notification.request.content.userInfo["thread_id"] as? String else { return }
        await MainActor.run {
            guard case .success(let model) = boot else { return }
            NSApp.activate()
            Task { do { try await model.openThread(threadId) } catch { model.show(error) } }
        }
    }
}

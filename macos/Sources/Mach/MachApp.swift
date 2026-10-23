import AppKit
import SwiftUI
import UserNotifications

@main
struct MachApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate

    var body: some Scene {
        Window("Mach", id: "main") {
            ContentView(model: delegate.model)
        }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    let model = AppModel(engine: PreviewEngine())
    private var debugServer: DebugServer?
    private var keyMonitor: Any?

    // The delegate must be set before launch finishes, or a click that
    // launched the app is never delivered.
    func applicationWillFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = self
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [model] event in
            model.handleKey(KeyChord.name(of: event)) ? nil : event
        }
        let port = ProcessInfo.processInfo.environment["MACH_DEBUG_PORT"].flatMap(UInt16.init) ?? 7366
        debugServer = DebugServer(port: port, model: model)
        Task {
            await model.load()
            do {
                model.notificationsAllowed = try await UNUserNotificationCenter.current()
                    .requestAuthorization(options: [.alert, .sound, .badge])
            } catch {
                model.notificationsAllowed = false
                model.lastError = "notification authorization: \(error)"
            }
        }
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
            model.openThread(threadId)
            NSApp.activate()
        }
    }
}

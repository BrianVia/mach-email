import AppKit
import SwiftUI
import UserNotifications

@main
struct MachApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate

    var body: some Scene {
        Window("Mach", id: "main") {
            Group {
                switch delegate.boot {
                case .success(let model): ContentView(model: model)
                case .failure(let error):
                    ContentUnavailableView("Mach couldn't start", systemImage: "exclamationmark.triangle", description: Text(String(describing: error)))
                        .frame(minWidth: 600, minHeight: 400)
                }
            }
            .background(WindowStateKeeper())
        }
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("Add Account…") {
                    guard case .success(let model) = delegate.boot else { return }
                    Task { await model.addAccount(loginHint: nil) }
                }
                .keyboardShortcut("n")
            }
        }
        SwiftUI.Settings { SettingsView() }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    let boot: Result<AppModel, Error> = Result {
        AppDelegate.yieldToRunningCopy()
        return AppModel(engine: try Engine())
    }
    private var debugServer: DebugServer?
    private var keyMonitor: Any?

    /// A second launch of the same Mach.app focuses the first one and quits
    /// before starting an engine. Copies at other paths (dev builds) run side by side.
    private static func yieldToRunningCopy() {
        let me = NSRunningApplication.current
        let bundle = Bundle.main.bundleURL.standardizedFileURL
        guard let bundleId = Bundle.main.bundleIdentifier,
            let running = NSRunningApplication.runningApplications(withBundleIdentifier: bundleId)
                .first(where: { $0 != me && $0.bundleURL?.standardizedFileURL == bundle })
        else { return }
        running.activate(options: .activateAllWindows)
        exit(0)
    }

    // The delegate must be set before launch finishes, or a click that
    // launched the app is never delivered.
    func applicationWillFinishLaunching(_ notification: Notification) {
        UNUserNotificationCenter.current().delegate = self
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        guard case .success(let model) = boot else { return }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [model] event in
            Self.shouldIntercept(event) && KeyChord.chord(for: event).map(model.handleKey) == true ? nil : event
        }
        if let port = Self.debugPort { debugServer = DebugServer(port: port, model: model) }
        Task { await model.listen() }
        Task {
            await model.load()
            // The engine's own loop first syncs 15s after launch; sync now so
            // the status pill reflects reality within seconds.
            _ = try? await model.engine.syncNow()
        }
        Task {
            do {
                model.notificationsAllowed = try await UNUserNotificationCenter.current()
                    .requestAuthorization(options: [.alert, .sound, .badge])
            } catch {
                model.notificationsAllowed = false
                model.engine.log("warn", "notification authorization: \(error)")
            }
            await model.refreshNotificationAuthorization()
        }
    }

    /// Off unless MACH_DEBUG_PORT is set; debug builds default to 7366.
    private static var debugPort: UInt16? {
        if let port = ProcessInfo.processInfo.environment["MACH_DEBUG_PORT"] { return UInt16(port) }
        #if DEBUG
        return 7366
        #else
        return nil
        #endif
    }

    /// The user may have changed the permission in System Settings meanwhile.
    func applicationDidBecomeActive(_ notification: Notification) {
        guard case .success(let model) = boot else { return }
        Task { await model.refreshNotificationAuthorization() }
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
        guard response.actionIdentifier == UNNotificationDefaultActionIdentifier else { return }
        let userInfo = response.notification.request.content.userInfo
        let info = ["thread_id", "account", "split"].reduce(into: [String: String]()) { $0[$1] = userInfo[$1] as? String }
        await MainActor.run {
            guard case .success(let model) = boot else { return }
            Task { await model.openFromNotification(info) }
        }
    }
}

import AppKit
import SwiftUI
import UserNotifications

@main
struct MachApp: App {
    @NSApplicationDelegateAdaptor private var delegate: AppDelegate

    var body: some Scene {
        Window("Mach", id: "main") {
            switch delegate.boot {
            case .launching:
                ProgressView("Opening mail…").frame(minWidth: 960, minHeight: 600)
            case .ready(let model): ContentView(model: model)
            case .failed(let error):
                ContentUnavailableView("Mach couldn't start", systemImage: "exclamationmark.triangle", description: Text(String(describing: error)))
                    .frame(minWidth: 600, minHeight: 400)
            }
        }
        .commands { MenuCommands(delegate: delegate) }
        SwiftUI.Settings { SettingsView() }
    }
}

enum Boot {
    case launching
    case ready(AppModel)
    case failed(Error)
}

@Observable @MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    private(set) var boot = Boot.launching
    /// The engine opens its database with a blocking call, so it starts off
    /// the main thread while the window shows a placeholder.
    @ObservationIgnored private var booting: Task<AppModel, Error>?
    @ObservationIgnored private var debugServer: DebugServer?
    @ObservationIgnored private var keyMonitor: Any?
    @ObservationIgnored private var terminating = false

    var model: AppModel? { if case .ready(let model) = boot { model } else { nil } }

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
        Self.yieldToRunningCopy()
        booting = Task { AppModel(engine: try await Task.detached { try Engine() }.value) }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        Task {
            do {
                let model = try await booting!.value
                boot = .ready(model)
                start(model)
            } catch {
                boot = .failed(error)
            }
        }
    }

    private func start(_ model: AppModel) {
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [model] event in
            guard let chord = KeyChord.chord(for: event) else { return event }
            if let consumed = model.handleRouteKey(chord) { return consumed ? nil : event }
            return Self.shouldIntercept(event) && model.handleKey(chord) ? nil : event
        }
        if let port = Self.debugPort {
            debugServer = DebugServer(port: port, model: model)
            // Agents' copies must never pass for the owner's own app.
            NSApp.dockTile.badgeLabel = "TEST"
        }
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

    /// Off unless MACH_DEBUG_PORT is set, in every build.
    static var debugPort: UInt16? {
        ProcessInfo.processInfo.environment["MACH_DEBUG_PORT"].flatMap { UInt16($0) }
    }

    /// The user may have changed the permission in System Settings meanwhile.
    func applicationDidBecomeActive(_ notification: Notification) {
        guard let model else { return }
        Task { await model.refreshNotificationAuthorization() }
    }

    /// Text fields keep their keys; Esc and command chords (the composer's
    /// cmd+s and cmd+enter) still reach the keymap, which passes unbound ones on.
    private static func shouldIntercept(_ event: NSEvent) -> Bool {
        guard let editor = event.window?.firstResponder as? NSTextView, editor.isEditable else { return true }
        return event.keyCode == 53 || !event.modifierFlags.isDisjoint(with: [.command, .control])
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    /// ⌘Q and closing the window save an open or still-saving draft first,
    /// but never hold the quit longer than a few seconds.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let model, model.hasUnsavedDraft else { return .terminateNow }
        model.engine.log("info", "quit waits for the open draft to save")
        terminating = true
        Task {
            await model.saveDraftsBeforeQuit()
            finishTerminating()
        }
        Task {
            try? await Task.sleep(for: .seconds(3))
            if terminating { model.engine.log("warn", "quit before the draft finished saving") }
            finishTerminating()
        }
        return .terminateLater
    }

    private func finishTerminating() {
        guard terminating else { return }
        terminating = false
        NSApp.reply(toApplicationShouldTerminate: true)
    }

    /// SwiftUI restores the frame saved under the window's autosave name but
    /// does not write it on resize or quit, so save it on the way out.
    func applicationWillTerminate(_ notification: Notification) {
        for window in NSApp.windows where !window.frameAutosaveName.isEmpty {
            window.saveFrame(usingName: window.frameAutosaveName)
        }
    }

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
        // A click that launched the app arrives while the engine is still starting.
        guard let model = try? await MainActor.run(body: { booting })?.value else { return }
        await model.openFromNotification(info)
    }
}

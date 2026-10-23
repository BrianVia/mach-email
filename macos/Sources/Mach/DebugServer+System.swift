import AppKit

/// Debug-only hooks for notifications, sign-in, and window state. None of
/// them talk to Gmail or change account files.
extension DebugServer {
    static let systemActionNames = ["simulate_mail_synced", "fake_reauth", "notifications_hint", "about", "window"]

    private var mainWindow: NSWindow? { NSApp.windows.first { $0.canBecomeMain } }

    func systemState() -> [String: Any] {
        let split = WindowStateKeeper.splitView(in: mainWindow?.contentView)
        return [
            "connection": model.connection.rawValue,
            "inbox_unread": model.inboxUnreadCount,
            "notifications_enabled": NotificationPreferences.enabled,
            "notifications_hint_visible": model.showsNotificationsHint,
            "needs_reauth": model.status?.needsReauth ?? [],
            "signing_in": model.system.signingIn,
            "window_frame": mainWindow.map { NSStringFromRect($0.frame) } ?? NSNull(),
            "sidebar_width": split?.arrangedSubviews.first.map { $0.frame.width } ?? NSNull(),
            "split_autosave_name": split?.autosaveName ?? NSNull(),
            "bundle_version": Bundle.main.infoDictionary?["CFBundleShortVersionString"] ?? NSNull(),
        ]
    }

    func systemAction(_ name: String, _ object: [String: Any]) async -> [String: Any] {
        switch name {
        case "simulate_mail_synced":
            // Real inbox rows so a click on the banner opens a real thread.
            let count = object["count"] as? Int ?? 1
            let inbox = (try? await model.engine.listThreads(label: "INBOX", limit: count)) ?? []
            let decision = await model.notifyNewMail(inbox)
            return ["decision": decision, "thread_ids": inbox.map(\.id), "app_active": NSApp.isActive, "system": systemState()]
        case "fake_reauth":
            // In memory only; the next account-status refresh drops it.
            if let email = object["email"] as? String {
                model.status?.needsReauth.append(email)
            } else if let status = try? model.engine.accountStatus() {
                model.status = status
            }
        case "notifications_hint":
            if object["reset"] as? Bool == true {
                UserDefaults.standard.removeObject(forKey: NotificationPreferences.hintDismissedKey)
                model.system.notificationsHintDismissed = false
            }
        case "about":
            if object["close"] as? Bool == true {
                NSApp.windows.first { $0.isVisible && !$0.canBecomeMain && $0.className.contains("About") }?.close()
            } else {
                NSApp.activate()
                NSApp.orderFrontStandardAboutPanel(nil)
            }
            try? await Task.sleep(for: .milliseconds(300))
            return ["windows": NSApp.windows.filter(\.isVisible).map { "\($0.className) \($0.title)" }]
        case "window":
            if let frame = object["frame"] as? [Double], frame.count == 4 {
                mainWindow?.setFrame(NSRect(x: frame[0], y: frame[1], width: frame[2], height: frame[3]), display: true)
            }
            if let width = object["sidebar"] as? Double, let split = WindowStateKeeper.splitView(in: mainWindow?.contentView) {
                split.setPosition(width, ofDividerAt: 0)
            }
            try? await Task.sleep(for: .milliseconds(300))
        default: break
        }
        return ["system": systemState()]
    }
}

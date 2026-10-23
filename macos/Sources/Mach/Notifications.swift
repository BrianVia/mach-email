import AppKit
import SwiftUI
import UserNotifications

/// One banner to post for a `mail-synced` event, as ui/src/lib/notify.ts batches them.
struct NewMailNotification: Equatable {
    var title: String
    var body: String?
    var thread: ThreadSummary

    /// Up to three threads get their own banner; the rest fold into "N more",
    /// which opens the newest thread.
    static func batch(_ threads: [ThreadSummary]) -> [NewMailNotification] {
        var batch = threads.prefix(3).map { thread in
            NewMailNotification(
                title: senderName(thread.participants.first ?? "New message"),
                body: thread.subject.isEmpty ? thread.snippet : thread.subject,
                thread: thread)
        }
        if threads.count > 3 {
            batch.append(NewMailNotification(title: "\(threads.count - 3) more new messages", body: nil, thread: threads[0]))
        }
        return batch
    }

    static func senderName(_ from: String) -> String {
        guard let match = from.wholeMatch(of: #/\s*"?([^"<]+?)"?\s*<.+>/#) else { return from }
        return match.1.trimmingCharacters(in: .whitespaces)
    }
}

enum NotificationPreferences {
    static let enabledKey = "mach.notifications"
    static let hintDismissedKey = "mach.notificationsHintDismissed"

    static var enabled: Bool { UserDefaults.standard.object(forKey: enabledKey) as? Bool ?? true }

    static var settingsURL: URL {
        URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=\(Bundle.main.bundleIdentifier ?? "")")!
    }
}

extension AppModel {
    func refreshNotificationAuthorization() async {
        system.notificationAuthorization = await UNUserNotificationCenter.current().notificationSettings().authorizationStatus
    }

    /// Posts banners for new inbox threads unless the user is already looking
    /// at the inbox. Returns what happened, for the debug server.
    @discardableResult
    func notifyNewMail(_ threads: [ThreadSummary]) async -> String {
        guard !threads.isEmpty else { return "skipped: no new threads" }
        guard NotificationPreferences.enabled else { return "skipped: notifications are off in Settings" }
        if NSApp.isActive, case .list = route, list.label == "INBOX" { return "skipped: app is focused on the inbox" }
        await refreshNotificationAuthorization()
        switch system.notificationAuthorization {
        case .authorized, .provisional: break
        case .denied: return "blocked: authorization denied"
        default: return "blocked: authorization not determined"
        }
        var posted = 0
        for notification in NewMailNotification.batch(threads) {
            let content = UNMutableNotificationContent()
            content.title = notification.title
            content.body = notification.body ?? ""
            content.sound = .default
            content.threadIdentifier = notification.thread.accountId
            content.userInfo = [
                "thread_id": notification.thread.id,
                "account": notification.thread.accountId,
                "split": Split.of(notification.thread.labelIds).rawValue,
            ]
            do {
                try await UNUserNotificationCenter.current().add(
                    UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
                posted += 1
            } catch {
                engine.log("warn", "posting notification: \(error)")
            }
        }
        return "posted \(posted)"
    }

    /// A notification click: bring the window forward and show the thread in
    /// the inbox it arrived in, so Esc lands on its row.
    func openFromNotification(_ userInfo: [String: String]) async {
        guard let threadId = userInfo["thread_id"] else { return }
        NSApp.activate()
        if let window = NSApp.windows.first(where: \.canBecomeMain) {
            if window.isMiniaturized { window.deminiaturize(nil) }
            window.makeKeyAndOrderFront(nil)
        }
        do {
            if list.label != "INBOX" { try await openLabel("INBOX") }
            if let split = userInfo["split"].flatMap(Split.init(rawValue:)) { selectSplit(split) }
            if let account = userInfo["account"], list.account.map({ $0 != account }) == true { selectAccount(nil) }
            try await openThread(threadId)
        } catch {
            show(error)
        }
    }

    var showsNotificationsHint: Bool {
        system.notificationAuthorization == .denied && NotificationPreferences.enabled && !system.notificationsHintDismissed
    }

    func dismissNotificationsHint(openSettings: Bool) {
        if openSettings { NSWorkspace.shared.open(NotificationPreferences.settingsURL) }
        system.notificationsHintDismissed = true
        UserDefaults.standard.set(true, forKey: NotificationPreferences.hintDismissedKey)
    }
}

struct NotificationsHint: View {
    let model: AppModel

    var body: some View {
        SystemBanner(symbol: "bell.slash", text: "Turn on notifications in System Settings to hear about new mail.") {
            Button("Open Settings") { model.dismissNotificationsHint(openSettings: true) }
            Button("Not Now", systemImage: "xmark") { model.dismissNotificationsHint(openSettings: false) }
                .labelStyle(.iconOnly)
                .buttonStyle(.borderless)
                .help("Hide this hint")
        }
    }
}

struct SettingsView: View {
    @AppStorage(NotificationPreferences.enabledKey) private var notifications = true

    var body: some View {
        Form {
            Toggle("Show notifications for new mail", isOn: $notifications)
            Button("Notification Settings…") { NSWorkspace.shared.open(NotificationPreferences.settingsURL) }
        }
        .formStyle(.grouped)
        .frame(width: 420)
        .fixedSize()
    }
}

import AppKit
import SwiftUI
import UserNotifications

/// App-level state outside the mail views: notification permission, the
/// sign-in flow, and the sidebar's inbox count.
struct SystemState {
    var notificationAuthorization: UNAuthorizationStatus = .notDetermined
    var notificationsHintDismissed = UserDefaults.standard.bool(forKey: NotificationPreferences.hintDismissedKey)
    var signingIn = false
    /// Unread inbox threads per account, over the newest `initialListLimit`.
    var inboxUnread: [String: Int] = [:]
}

enum Connection: String {
    /// No sync has reported back for some account yet.
    case connecting
    case live
    case offline
}

extension AppModel {
    var connection: Connection {
        guard !accounts.isEmpty else { return .offline }
        let reauth = Set(status?.needsReauth ?? [])
        if accounts.contains(where: { syncOkByAccount[$0] == false || reauth.contains($0) }) { return .offline }
        return accounts.allSatisfy { syncOkByAccount[$0] == true } ? .live : .connecting
    }

    /// The store keeps no unread count for system labels, so the inbox badge
    /// counts unread threads among the inbox rows the app lists.
    var inboxUnreadCount: Int {
        list.account.map { system.inboxUnread[$0] ?? 0 } ?? system.inboxUnread.values.reduce(0, +)
    }

    func recountInbox() async {
        let threads = list.label == "INBOX"
            ? list.threads
            : try? await engine.listThreads(label: "INBOX", limit: Self.initialListLimit)
        guard let threads else { return }
        system.inboxUnread = Dictionary(grouping: threads.filter(\.unread), by: \.accountId).mapValues(\.count)
    }
}

/// An inline bar across the top of the thread list, like Mail's account warnings.
struct SystemBanner<Actions: View>: View {
    let symbol: String
    let text: String
    @ViewBuilder let actions: Actions

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Image(systemName: symbol).foregroundStyle(.orange).imageScale(.large)
                Text(text).lineLimit(2).frame(maxWidth: .infinity, alignment: .leading)
                actions.controlSize(.small)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            Divider()
        }
        .background(.bar)
    }
}

struct SystemBanners: View {
    let model: AppModel

    var body: some View {
        VStack(spacing: 0) {
            ForEach(model.status?.needsReauth ?? [], id: \.self) { email in
                ReauthBanner(model: model, email: email)
            }
            if model.showsNotificationsHint { NotificationsHint(model: model) }
        }
    }
}

/// Saves the main window's frame and the sidebar's width under fixed names
/// so AppKit restores both on the next launch.
struct WindowStateKeeper: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView { Probe() }
    func updateNSView(_ nsView: NSView, context: Context) {}

    private final class Probe: NSView {
        override func viewDidMoveToWindow() {
            guard let window else { return }
            window.setFrameAutosaveName("Mach Main Window")
            // The split view exists once SwiftUI has laid out the columns.
            DispatchQueue.main.async {
                guard let split = WindowStateKeeper.splitView(in: window.contentView) else { return }
                split.autosaveName = "Mach Main Split"
            }
        }
    }

    static func splitView(in view: NSView?) -> NSSplitView? {
        guard let view else { return nil }
        if let split = view as? NSSplitView { return split }
        return view.subviews.lazy.compactMap(splitView).first
    }
}

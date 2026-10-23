import Foundation
import Observation

/// Single source of UI state. Keys, menu items, the debug server, and
/// notification clicks all funnel into `perform(_:)` or `openThread(_:)`.
@Observable @MainActor
final class AppModel {
    enum Action: String, CaseIterable {
        case selectNext = "select_next"
        case selectPrev = "select_prev"
        case openThread = "open_thread"
        case backToList = "back_to_list"
        case refresh
    }

    // Stand-in for the keymap the bridge will resolve from config/default-keymap.toml.
    static let keymap: [String: Action] = [
        "j": .selectNext, "down": .selectNext,
        "k": .selectPrev, "up": .selectPrev,
        "enter": .openThread, "l": .openThread,
        "esc": .backToList,
    ]

    let engine: any MailEngine
    var account: AccountStatus?
    var labels: [MailLabel] = []
    var selectedLabelId: String? = "INBOX"
    var threads: [ThreadSummary] = []
    var selectedThreadId: String?
    var openedThreadId: String?
    var lastError: String?
    /// nil while the macOS permission prompt is unanswered.
    var notificationsAllowed: Bool?

    init(engine: any MailEngine) {
        self.engine = engine
    }

    var view: String { openedThreadId == nil ? "list" : "thread" }
    var openedThread: ThreadSummary? { threads.first { $0.id == openedThreadId } }

    func load() async {
        do {
            account = try await engine.accountStatus()
            labels = try await engine.listLabels()
            try await loadThreads()
        } catch {
            lastError = String(describing: error)
        }
    }

    func selectLabel(_ id: String?) async {
        guard id != selectedLabelId else { return }
        selectedLabelId = id
        openedThreadId = nil
        do { try await loadThreads() } catch { lastError = String(describing: error) }
    }

    private func loadThreads() async throws {
        threads = try await engine.listThreads(label: selectedLabelId ?? "INBOX")
        if !threads.contains(where: { $0.id == selectedThreadId }) {
            selectedThreadId = threads.first?.id
        }
    }

    func perform(_ action: Action) {
        switch action {
        case .selectNext: moveSelection(by: 1)
        case .selectPrev: moveSelection(by: -1)
        case .openThread: if let selectedThreadId { openThread(selectedThreadId) }
        case .backToList: openedThreadId = nil
        case .refresh: Task { await load() }
        }
    }

    /// Returns false when no binding matched, so the event passes through.
    func handleKey(_ chord: String) -> Bool {
        guard let action = Self.keymap[chord] else { return false }
        perform(action)
        return true
    }

    func openThread(_ id: String) {
        selectedThreadId = id
        openedThreadId = id
    }

    private func moveSelection(by delta: Int) {
        guard !threads.isEmpty else { return }
        let current = threads.firstIndex { $0.id == selectedThreadId } ?? -delta
        selectedThreadId = threads[min(max(current + delta, 0), threads.count - 1)].id
    }
}

import AppKit
import Foundation
import Observation

/// Gmail's inbox categories folded into four tabs, like ui/src/lib/split.ts.
enum Split: String, CaseIterable, Identifiable {
    case important, other, updates, newsletters

    var id: String { rawValue }
    var title: String { rawValue.capitalized }

    static func of(_ labelIds: [String]) -> Split {
        if labelIds.contains("CATEGORY_UPDATES") { return .updates }
        if labelIds.contains("CATEGORY_PROMOTIONS") || labelIds.contains("CATEGORY_FORUMS") { return .newsletters }
        if labelIds.contains("IMPORTANT") || labelIds.contains("CATEGORY_PERSONAL")
            || !labelIds.contains(where: { $0.hasPrefix("CATEGORY_") })
        {
            return .important
        }
        return .other
    }
}

/// The thread list behind every route. It stays loaded while a thread is
/// open, so Esc needs no reload and keeps the selection.
struct ListState {
    var label = "INBOX"
    /// nil shows every account.
    var account: String?
    /// Every thread loaded for `label`, before the account and split filters.
    var threads: [ThreadSummary] = []
    var selection: ThreadSummary.ID?
    var limit = AppModel.initialListLimit
}

struct ReaderState {
    var thread: ThreadSummary
    var messages: [Message]
    var selectedMessage: Int
    var bodyFetchError: String?
    var viewer: ImageViewerState?
    var unsubscribe: UnsubscribeConfirm?
    /// Message ids whose blocked remote images the user chose to show.
    var shownRemoteImages: Set<String> = []
    var inviteReplies: [String: InviteReply] = [:]

    var currentMessage: Message? { messages.indices.contains(selectedMessage) ? messages[selectedMessage] : nil }
}

/// What owns the keyboard and the detail column.
enum Route {
    case list
    case thread(ReaderState)
    case composer(ComposerModel)
    case search
    case activity(ActivityState)
    case scheduled(ScheduledState)

    var name: String {
        switch self {
        case .list: "list"
        case .thread: "thread"
        case .composer: "composer"
        case .search: "search"
        case .activity: "activity"
        case .scheduled: "scheduled"
        }
    }

    var threadId: String? { if case .thread(let reader) = self { reader.thread.id } else { nil } }

    var mode: KeyMode {
        switch self {
        case .thread: .reading
        case .composer: .composing
        default: .normal
        }
    }
}

struct Notice: Equatable {
    var text: String
    var isError = false
    /// A saved file; clicking the notice reveals it in Finder.
    var path: String?
}

/// Single source of UI state. Keys, menus, the debug server, and notification
/// clicks all funnel into `handleKey(_:)` or `run(_:)`.
@Observable @MainActor
final class AppModel {
    nonisolated static let initialListLimit = 1000
    static let systemLabels: [(id: String, name: String, symbol: String)] = [
        ("INBOX", "Inbox", "tray"),
        ("STARRED", "Starred", "star"),
        ("SENT", "Sent", "paperplane"),
        ("DRAFT", "Drafts", "doc"),
        ("SCHEDULED", "Scheduled", "clock"),
        ("DONE", "Done", "checkmark.circle"),
        ("SNOOZED", "Snoozed", "moon.zzz"),
        ("TRASH", "Trash", "trash"),
        ("SPAM", "Spam", "xmark.octagon"),
        ("ALL", "All Mail", "archivebox"),
    ]
    /// Dispatched straight to the engine. Anything else the keymap resolves
    /// to is either handled below or not built yet.
    private static let engineActions: Set<String> = ["star", "mark_read", "add_label", "remove_label", "undo", "redo"]
    private static let removingActions: Set<String> = ["archive", "trash", "mute"]

    let engine: Engine
    /// Every navigation goes through this setter, so a composer that loses
    /// the screen to a menu item, a notification, or a keymap action is saved
    /// like Esc saves it.
    var route: Route = .list {
        didSet {
            if case .composer(let outgoing) = oldValue, composer !== outgoing { autosave(outgoing) }
            if oldValue.name != route.name || oldValue.threadId != route.threadId { navigation += 1 }
            // A search lives while its results, a thread opened from them, or a
            // composer over either is on screen.
            switch route {
            case .search, .thread, .composer: break
            default: search = nil
            }
        }
    }
    var search: SearchState?
    /// The command palette floats over whatever route is showing.
    var palette: PaletteState?
    var list = ListState()
    var split: Split = Split(rawValue: UserDefaults.standard.string(forKey: "mach.inboxSplit") ?? "") ?? .important {
        didSet { UserDefaults.standard.set(split.rawValue, forKey: "mach.inboxSplit") }
    }
    var labels: [MailLabel] = []
    /// Lowercased sender addresses whose remote images always load, like the Tauri app's `mach.remoteImageAllow`.
    var remoteImageAllow = Set(UserDefaults.standard.stringArray(forKey: "mach.remoteImageAllow") ?? []) {
        didSet { UserDefaults.standard.set(remoteImageAllow.sorted(), forKey: "mach.remoteImageAllow") }
    }
    var status: AccountStatus?
    var settings: Settings?
    var outbox = OutboxSummary(pending: 0, failed: 0, lastError: nil)
    var syncOkByAccount: [String: Bool] = [:]
    var notice: Notice?
    var chordBuffer = ""
    var continuations: [Continuation] = []
    var lastMailSyncedAt: Date?
    /// nil while the macOS permission prompt is unanswered.
    var notificationsAllowed: Bool?
    var system = SystemState()
    private var noticeGeneration = 0
    /// Bumped by each navigation, so an open whose await finishes after a
    /// newer one started drops its result.
    @ObservationIgnored var navigation = 0
    @ObservationIgnored var draftSaves: Task<Void, Never>?
    @ObservationIgnored var pendingDraftSaves = 0
    @ObservationIgnored private var loadingOlder = false

    init(engine: Engine) {
        self.engine = engine
    }

    // MARK: Derived state

    var visibleThreads: [ThreadSummary] {
        let threads = list.account.map { account in list.threads.filter { $0.accountId == account } } ?? list.threads
        return list.label == "INBOX" ? threads.filter { Split.of($0.labelIds) == split } : threads
    }

    var reader: ReaderState? {
        get { if case .thread(let state) = route { state } else { nil } }
        set { if let newValue { route = .thread(newValue) } }
    }

    var accounts: [String] { status?.accounts ?? [] }
    var online: Bool { !accounts.isEmpty && accounts.allSatisfy { syncOkByAccount[$0] == true } }

    func accountLabel(_ email: String) -> String {
        status?.accountLabels[email] ?? settings?.accountLabels?[email] ?? email
    }

    func unreadCount(in split: Split) -> Int {
        list.threads.filter { (list.account == nil || $0.accountId == list.account) && $0.unread && Split.of($0.labelIds) == split }.count
    }

    var title: String {
        switch composer?.background ?? route {
        case .thread(let reader): reader.thread.subject.isEmpty ? "(no subject)" : reader.thread.subject
        case .activity: "Activity"
        case .search: "Search"
        case .scheduled: "Scheduled"
        default: labelName(list.label)
        }
    }

    var subtitle: String {
        if case .scheduled(let state) = route { return "\(state.sends.count) scheduled" }
        if case .thread(let reader) = composer?.background ?? route {
            let people = reader.thread.participants
            return people.prefix(2).joined(separator: ", ") + (people.count > 2 ? " +\(people.count - 2)" : "")
        }
        if let search { return "\(search.results.count.formatted()) results" }
        if case .activity(let activity) = route { return "\(activity.entries.count) recent changes" }
        // The same rows the split tab counts, so "6 unread" here matches "Important 6".
        let visible = visibleThreads
        let unread = visible.reduce(0) { $0 + ($1.unread ? 1 : 0) }
        return "\(visible.count.formatted()) conversations" + (unread > 0 ? " · \(unread.formatted()) unread" : "")
            + (list.account.map { " · only \(accountLabel($0))" } ?? "")
    }

    func labelName(_ id: String) -> String {
        Self.systemLabels.first { $0.id == id }?.name ?? labels.first { $0.id == id }?.name ?? id
    }

    /// One row per user label name: the store matches same-named labels across
    /// accounts, so any account's id opens all of them.
    var userLabels: [MailLabel] {
        var byName: [String: MailLabel] = [:]
        for label in labels where !label.system && !label.name.hasPrefix("MACH/") {
            if var seen = byName[label.name] {
                if let count = label.unreadCount { seen.unreadCount = (seen.unreadCount ?? 0) + count }
                byName[label.name] = seen
            } else {
                byName[label.name] = label
            }
        }
        return byName.values.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    // MARK: Loading

    func load() async {
        do {
            status = try await engine.accountStatus()
            let settings = try await engine.settings()
            self.settings = settings
            if let problem = settings.problem { show(problem, isError: true) }
            labels = try await engine.listLabels()
            list.threads = try await engine.listThreads(label: list.label, limit: list.limit)
            list.selection = visibleThreads.first?.id
        } catch {
            show(error)
        }
        await refreshOutbox()
    }

    func listen() async {
        for await event in engine.events {
            switch event {
            case .mailSynced(let payload):
                lastMailSyncedAt = Date()
                engine.log("info", "mail-synced \(payload.account) new=\(payload.newThreads.count)")
                if let next = try? await engine.listLabels() { labels = next }
                await refreshList()
                await reloadOpenThread()
                await notifyNewMail(payload.newThreads)
            case .syncStatus(let payload):
                syncOkByAccount[payload.account] = payload.ok
                if !payload.ok, let next = try? await engine.accountStatus() { status = next }
                await refreshOutbox()
            }
        }
    }

    func refreshOutbox() async {
        if let summary = try? await engine.outboxSummary() { outbox = summary }
    }

    /// Reloads the current label and keeps the selected thread selected, or
    /// the row now in its place when it left the list.
    func refreshList() async {
        let before = list
        let oldIndex = visibleThreads.firstIndex { $0.id == before.selection } ?? 0
        guard let threads = try? await engine.listThreads(label: before.label, limit: before.limit),
            list.label == before.label
        else { return }
        list.threads = threads
        keepSelection(near: oldIndex)
    }

    private func keepSelection(near index: Int) {
        let visible = visibleThreads
        if visible.contains(where: { $0.id == list.selection }) { return }
        list.selection = visible.isEmpty ? nil : visible[min(index, visible.count - 1)].id
    }

    // MARK: Keyboard

    /// Returns false when nothing is bound, so the event reaches the responder chain.
    func handleKey(_ chord: String) -> Bool {
        if let handled = handleReaderKey(chord) { return handled }
        let sequence = chordBuffer.isEmpty ? chord : "\(chordBuffer) \(chord)"
        switch engine.resolveKey(mode: route.mode, chord: sequence, context: keyContext) {
        case .action(let action):
            clearChord()
            Task { await run(action) }
            return true
        case .prefix(let next):
            chordBuffer = sequence
            continuations = next
            return true
        case .unbound:
            guard !chordBuffer.isEmpty else { return false }
            clearChord()
            guard case .action(let action) = engine.resolveKey(mode: route.mode, chord: chord, context: keyContext) else {
                return true
            }
            Task { await run(action) }
            return true
        }
    }

    private func clearChord() {
        chordBuffer = ""
        continuations = []
    }

    var keyContext: KeyContext {
        if let composer { return KeyContext(currentDraft: composer.draft.id) }
        if scheduled != nil { return KeyContext() }
        if let reader {
            return KeyContext(selection: [reader.thread.id], currentThread: reader.thread.id, currentMessage: reader.currentMessage?.id)
        }
        if case .search = route {
            let selected = search?.selectedThread?.id
            return KeyContext(selection: selected.map { [$0] } ?? [], currentThread: selected)
        }
        let selected = visibleThreads.first { $0.id == list.selection }?.id
        return KeyContext(selection: selected.map { [$0] } ?? [], currentThread: selected)
    }

    // MARK: Actions

    func run(_ action: KeyAction) async {
        let fields = action.fields
        do {
            if try await runInScheduled(action) { return }
            switch action.kind {
            case "quit": return
            case "select_next": moveSelection(by: 1)
            case "select_prev": moveSelection(by: -1)
            case "open_thread": if let id = fields.id { try await openThread(id) }
            case "back_to_list" where composer != nil: escapeComposer()
            case "back_to_list": backToList()
            case "open_label": if let label = fields.labelId { try await openLabel(label) }
            case "show_activity": try await openActivity()
            case "undo_activity": if let id = fields.outboxId { try await undoActivity(id) }
            case "search": openSearch()
            case "inbox_split_important", "inbox_split_other", "inbox_split_updates", "inbox_split_newsletters":
                if list.label == "INBOX", let next = Split(rawValue: String(action.kind.dropFirst("inbox_split_".count))) {
                    selectSplit(next)
                }
            case "load_older": try await loadOlder()
            case "refresh": try await syncNow()
            case "unsubscribe": if let id = fields.messageId { try await beginUnsubscribe(messageId: id) }
            case "respond_to_invite":
                if let id = fields.messageId, let response = fields.response { await respondToInvite(messageId: id, response: response) }
            case "compose_new", "reply", "forward", "save_draft", "send_draft": try await runCompose(action)
            case "add_account": await addAccount(loginHint: nil)
            case let kind where Self.removingActions.contains(kind): try await remove(action)
            case let kind where Self.engineActions.contains(kind): try await mutate(action)
            default:
                engine.log("info", "action \(action.kind) is not in the native app yet")
                show("\(action.kind.replacingOccurrences(of: "_", with: " ").capitalized) isn't in the native app yet")
            }
        } catch {
            show(error)
        }
    }

    func moveSelection(by delta: Int) {
        if var state = reader {
            state.selectedMessage = min(max(state.selectedMessage + delta, 0), max(state.messages.count - 1, 0))
            reader = state
            return
        }
        let visible = visibleThreads
        guard !visible.isEmpty else { return }
        let current = visible.firstIndex { $0.id == list.selection } ?? (delta > 0 ? -1 : visible.count)
        list.selection = visible[min(max(current + delta, 0), visible.count - 1)].id
    }

    func selectSplit(_ next: Split) {
        split = next
        keepSelection(near: 0)
    }

    func selectAccount(_ account: String?) {
        list.account = account
        keepSelection(near: 0)
    }

    func openLabel(_ label: String) async throws {
        if label == "SCHEDULED" { return try await openScheduled() }
        navigation += 1
        let request = navigation
        let threads = try await engine.listThreads(label: label, limit: Self.initialListLimit)
        guard navigation == request else { return }
        list = ListState(label: label, account: list.account, threads: threads)
        list.selection = visibleThreads.first?.id
        route = .list
    }

    func openThread(_ id: String) async throws {
        navigation += 1
        let request = navigation
        let cached = try await engine.openThread(id, fetch: false)
        guard navigation == request else { return }
        if list.threads.contains(where: { $0.id == id }) { list.selection = id }
        if let index = search?.results.firstIndex(where: { $0.id == id }) { search?.selected = index }
        // The newest message is the one a reply or unsubscribe targets.
        route = .thread(ReaderState(thread: cached.thread, messages: cached.messages, selectedMessage: max(cached.messages.count - 1, 0)))
        if cached.thread.unread {
            markLocally(ids: [id]) { $0.unread = false }
            Task {
                _ = try? await engine.dispatch(KeyAction(["kind": "mark_read", "thread_ids": [id], "read": true]))
            }
        }
        if let next = (search?.results ?? visibleThreads).drop(while: { $0.id != id }).dropFirst().first {
            Task { _ = try? await engine.openThread(next.id, fetch: true) }
        }
        guard cached.messages.contains(where: { !$0.fetchedFull }) else { return }
        let full = try await engine.openThread(id, fetch: true)
        guard var state = reader, state.thread.id == id else { return }
        state.messages = full.messages
        state.bodyFetchError = full.bodyFetchError
        reader = state
        if let error = full.bodyFetchError { show("Couldn't fetch the full message, showing a preview (\(error))", isError: true) }
    }

    func refetchOpenThread() async {
        guard let id = reader?.thread.id else { return }
        do {
            let detail = try await engine.refetchThread(id)
            guard var state = reader, state.thread.id == id else { return }
            state.thread = detail.thread
            state.messages = detail.messages
            state.selectedMessage = min(state.selectedMessage, max(detail.messages.count - 1, 0))
            reader = state
        } catch {
            show(error)
        }
    }

    /// Re-reads the open thread from the store after a sync or a send and
    /// keeps the selected message selected.
    func reloadOpenThread() async {
        guard let id = reader?.thread.id, let cached = try? await engine.openThread(id, fetch: false) else { return }
        guard let state = reader, state.thread.id == id,
            cached.thread != state.thread || cached.messages.map(\.id) != state.messages.map(\.id)
        else { return }
        apply(cached, to: id)
        guard cached.messages.contains(where: { !$0.fetchedFull }), let full = try? await engine.openThread(id, fetch: true) else { return }
        apply(full, to: id)
    }

    private func apply(_ detail: ThreadDetail, to id: String) {
        guard var state = reader, state.thread.id == id else { return }
        let selected = state.currentMessage?.id
        let known = Dictionary(state.messages.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        state.thread = detail.thread
        state.messages = detail.messages.map { message in
            message.fetchedFull ? message : known[message.id].flatMap { $0.fetchedFull ? $0 : nil } ?? message
        }
        state.selectedMessage = state.messages.firstIndex { $0.id == selected } ?? max(state.messages.count - 1, 0)
        reader = state
    }

    func backToList() {
        if case .list = route { return }
        if search != nil, case .thread = route {
            route = .search
            return
        }
        route = .list
        Task { await refreshList() }
    }

    func loadOlder() async throws {
        guard let oldest = list.threads.last?.lastMessageAt else { return show("No older mail") }
        // Gmail can take minutes here, so the notice stays up until it answers.
        show(nil)
        notice = Notice(text: "Loading older mail…")
        guard !loadingOlder else { return }
        loadingOlder = true
        defer { loadingOlder = false }
        let label = list.label
        let stats = try await engine.loadOlder(label: label, before: oldest)
        let limit = list.limit + 200
        let threads = try await engine.listThreads(label: label, limit: limit)
        if list.label == label {
            list.threads = threads
            list.limit = limit
        }
        show(stats.fetched == 0 ? "No older mail" : "Loaded \(stats.fetched) older threads")
    }

    func syncNow() async throws {
        engine.log("info", "sync now requested")
        show("Syncing all accounts…")
        let report = try await engine.syncNow()
        if reader != nil { await refetchOpenThread() }
        await refreshList()
        if report.failed > 0 {
            show("Sync failed: \(report.lastError ?? "unknown error")", isError: true)
        } else {
            show("Synced \(report.synced) account\(report.synced == 1 ? "" : "s")")
        }
    }

    func retryFailedChanges() async {
        do {
            let retried = try await engine.retryOutbox()
            let report = try await engine.flushOutbox()
            await refreshOutbox()
            if report.failed > 0 {
                show(report.lastError ?? "Outbox retry failed", isError: true)
            } else {
                show("\(retried) failed change\(retried == 1 ? "" : "s") retried")
            }
        } catch {
            show(error)
        }
    }

    /// Archive, trash, and mute leave the list at once and dispatch in the
    /// background; the engine's optimistic write and outbox do the rest.
    private func remove(_ action: KeyAction) async throws {
        let removed = Set(action.fields.threadIds ?? [])
        guard !removed.isEmpty else { return }
        let index = visibleThreads.firstIndex { removed.contains($0.id) } ?? 0
        Task {
            do { _ = try await engine.dispatch(action) } catch { show(error) }
            await refreshOutbox()
        }
        show(Self.notice(for: action, count: removed.count))
        list.threads.removeAll { removed.contains($0.id) }
        let visible = visibleThreads
        list.selection = visible.isEmpty ? nil : visible[min(index, visible.count - 1)].id
        if var results = search {
            let index = results.results.firstIndex { removed.contains($0.id) } ?? results.selected
            results.results.removeAll { removed.contains($0.id) }
            results.selected = min(index, max(results.results.count - 1, 0))
            search = results
        }
        guard reader != nil else { return }
        if settings?.afterArchive != .list, let next = search.map({ $0.selectedThread?.id }) ?? list.selection {
            try await openThread(next)
        } else {
            route = search == nil ? .list : .search
        }
    }

    /// Star, read state, labels, undo: dispatch, mirror the flag locally so
    /// the row updates at once, then reload from the store.
    private func mutate(_ action: KeyAction) async throws {
        let ids = action.fields.threadIds ?? []
        switch action.kind {
        case "star": if let starred = action.fields.starred { markLocally(ids: ids) { $0.starred = starred } }
        case "mark_read": if let read = action.fields.read { markLocally(ids: ids) { $0.unread = !read } }
        default: break
        }
        let outcome = try await engine.dispatch(action)
        show(outcome.message.hasPrefix("nothing to") ? "Nothing to \(action.kind)" : Self.notice(for: action, count: outcome.changedThreads.count))
        await refreshList()
        await refreshOutbox()
    }

    private func markLocally(ids: [String], _ change: (inout ThreadSummary) -> Void) {
        for index in list.threads.indices where ids.contains(list.threads[index].id) {
            change(&list.threads[index])
        }
        if var state = reader, ids.contains(state.thread.id) {
            change(&state.thread)
            reader = state
        }
        if var results = search {
            for index in results.results.indices where ids.contains(results.results[index].id) {
                change(&results.results[index])
            }
            search = results
        }
    }

    /// The notice a finished mail action shows, e.g. "Archived" or "Starred 3 conversations".
    static func notice(for action: KeyAction, count: Int) -> String {
        let some = count > 1 ? " \(count) conversations" : ""
        let fields = action.fields
        switch action.kind {
        case "archive": return "Archived\(some)"
        case "trash": return "Moved\(some) to Trash"
        case "mute": return "Muted\(some)"
        case "star": return fields.starred == false ? "Unstarred\(some)" : "Starred\(some)"
        case "mark_read": return "Marked\(some) as \(fields.read == false ? "unread" : "read")"
        case "add_label": return count > 1 ? "Label added to\(some)" : "Label added"
        case "remove_label": return count > 1 ? "Label removed from\(some)" : "Label removed"
        case "undo": return "Undone"
        case "redo": return "Redone"
        default: return action.kind.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }

    // MARK: Notices

    func show(_ text: String?, isError: Bool = false) {
        noticeGeneration += 1
        notice = text.map { Notice(text: $0, isError: isError) }
        guard text != nil else { return }
        let generation = noticeGeneration
        Task {
            try? await Task.sleep(for: .seconds(4))
            if noticeGeneration == generation { notice = nil }
        }
    }

    func show(_ error: Error) {
        let text = (error as? LocalizedError)?.errorDescription ?? String(describing: error)
        engine.log("warn", text)
        show(text, isError: true)
    }
}

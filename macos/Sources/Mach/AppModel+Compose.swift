import AppKit
import Foundation
import Observation

// Codable mirrors of `Draft`, `DraftAttachment`, and `ScheduledSend` in
// crates/mach-desktop/ui/src/lib/ipc.ts.

struct DraftAttachment: Codable, Sendable, Hashable {
    var path: String
    var filename: String
    var mimeType: String

    var json: [String: Any] { ["path": path, "filename": filename, "mime_type": mimeType] }
}

struct Draft: Codable, Sendable {
    var accountId: String
    var id: String
    var threadId: String?
    var inReplyToMessageId: String?
    var to: [String]
    var cc: [String]
    var bcc: [String]
    var subject: String
    var bodyMd: String
    var attachments: [DraftAttachment]
    var updatedAt: Date
}

struct DraftOutcome: Decodable, Sendable {
    struct Payload: Decodable, Sendable { var draft: Draft? }
    var data: Payload?
}

struct ScheduledSend: Codable, Sendable, Identifiable, Hashable {
    var sendLaterId: String
    var draftId: String
    var sendAt: Date
    var subject: String
    var to: [String]
    var accountId: String
    var id: String { sendLaterId }
}

struct SendLaterPreset: Sendable, Hashable {
    var label: String
    var at: Date
}

/// A `;name` being typed in the body, as ui/src/lib/snippet.ts detects it.
/// Offsets are UTF-16, the unit NSTextView ranges use.
struct SnippetToken: Equatable {
    var start: Int
    var query: String

    static func detect(in text: String, cursor: Int) -> SnippetToken? {
        let before = (text as NSString).substring(to: min(cursor, (text as NSString).length)) as NSString
        let start = before.range(of: ";", options: .backwards).location
        guard start != NSNotFound else { return nil }
        if start > 0, !before.substring(with: NSRange(location: start - 1, length: 1)).allSatisfy(\.isWhitespace) { return nil }
        let query = before.substring(from: start + 1)
        guard !query.contains(where: \.isWhitespace) else { return nil }
        return SnippetToken(start: start, query: query)
    }
}

/// The open composer. A class so typing mutates it in place instead of
/// rewriting `AppModel.route` (and re-rendering the window) per keystroke.
@Observable @MainActor
final class ComposerModel: Identifiable {
    var draft: Draft
    /// Where Esc, send, and discard return to.
    let background: Route
    var to: String
    var cc: String
    var bcc: String
    var subject: String
    var body: String
    var attachments: [DraftAttachment]
    let snippets: [(name: String, value: String)]
    let presets: [SendLaterPreset]
    var snippet: SnippetToken?
    var snippetSelection = 0
    /// Sent, scheduled, or discarded: leaving it must not save it again.
    @ObservationIgnored var finished = false
    /// Set by the body editor so snippet insertion goes through the text
    /// system, which keeps undo and places the cursor.
    @ObservationIgnored weak var bodyView: NSTextView?

    init(draft: Draft, background: Route, snippets: [String: String], presets: [SendLaterPreset]) {
        self.draft = draft
        self.background = background
        to = draft.to.joined(separator: ", ")
        cc = draft.cc.joined(separator: ", ")
        bcc = draft.bcc.joined(separator: ", ")
        subject = draft.subject
        body = draft.bodyMd
        attachments = draft.attachments
        self.snippets = snippets.sorted { $0.key < $1.key }.map { (name: $0.key, value: $0.value) }
        self.presets = presets
    }

    var title: String {
        if draft.inReplyToMessageId != nil { return "Reply" }
        if draft.subject.hasPrefix("Fwd:") { return "Forward" }
        return "New Message"
    }

    var snippetMatches: [(name: String, value: String)] {
        guard let query = snippet?.query.lowercased() else { return [] }
        return snippets.filter { query.isEmpty || $0.name.lowercased().contains(query) }
    }

    func updateSnippet(cursor: Int) {
        snippet = SnippetToken.detect(in: body, cursor: cursor)
        snippetSelection = 0
    }

    func moveSnippetSelection(by delta: Int) {
        snippetSelection = min(max(snippetSelection + delta, 0), max(snippetMatches.count - 1, 0))
    }

    func insertSnippet(at index: Int? = nil) {
        let matches = snippetMatches
        guard let token = snippet, matches.indices.contains(index ?? snippetSelection) else { return }
        let value = matches[index ?? snippetSelection].value
        if let view = bodyView {
            // The caret may have left the token since it was typed.
            let cursor = view.selectedRange().location
            guard let current = SnippetToken.detect(in: view.string, cursor: cursor), current.start == token.start, cursor > token.start else {
                snippet = nil
                return
            }
            view.insertText(value, replacementRange: NSRange(location: token.start, length: cursor - token.start))
            view.window?.makeFirstResponder(view)
        } else {
            let text = body as NSString
            let cursor = token.start + 1 + (token.query as NSString).length
            body = text.replacingCharacters(in: NSRange(location: token.start, length: cursor - token.start), with: value)
        }
        snippet = nil
    }

    var patch: [String: Any] {
        func recipients(_ field: String) -> [String] {
            field.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        }
        return [
            "to": recipients(to), "cc": recipients(cc), "bcc": recipients(bcc),
            "subject": subject, "body_md": body, "attachments": attachments.map(\.json),
        ]
    }
}

struct ScheduledState {
    var sends: [ScheduledSend]
    var selection: ScheduledSend.ID?

    var selected: ScheduledSend? { sends.first { $0.id == selection } ?? sends.first }
}

extension AppModel {
    var composer: ComposerModel? {
        if case .composer(let composer) = route { composer } else { nil }
    }

    var scheduled: ScheduledState? {
        get { if case .scheduled(let state) = route { state } else { nil } }
        set { if let newValue { route = .scheduled(newValue) } }
    }

    // MARK: Composer

    /// compose_new, reply, forward, and the composing-mode keymap actions.
    func runCompose(_ action: KeyAction) async throws {
        var object = (try JSONSerialization.jsonObject(with: action.json) as? [String: Any]) ?? [:]
        switch action.kind {
        case "compose_new":
            if !(object["account"] is String) { object["account"] = composeAccount ?? NSNull() }
            openComposer(try await engine.dispatchDraft(KeyAction(object)))
        case "reply", "forward":
            // The list has no current message; reply to the selected thread's newest one.
            if !(object["message_id"] is String), let thread = keyContext.currentThread {
                object["message_id"] = try await engine.openThread(thread, fetch: false).messages.last?.id
            }
            guard object["message_id"] is String else { return show("Open a message to \(action.kind)") }
            openComposer(try await engine.dispatchDraft(KeyAction(object)))
        case "save_draft":
            guard let composer else { return }
            try await saveDraft(composer)
            show("Draft saved")
        case "send_draft":
            if let composer { try await send(composer) }
        default:
            break
        }
    }

    /// The account filter, else the configured default (the engine applies
    /// it when `account` is null), else the selected thread's account.
    private var composeAccount: String? {
        if let account = list.account { return account }
        if accounts.count == 1 || settings?.accountLabels?["default"] != nil { return nil }
        return visibleThreads.first { $0.id == list.selection }?.accountId
    }

    func openComposer(_ draft: Draft) {
        let snippets = (try? engine.snippets()) ?? [:]
        let presets = (try? engine.sendLaterPresets()) ?? []
        route = .composer(ComposerModel(draft: draft, background: composer?.background ?? route, snippets: snippets, presets: presets))
    }

    @discardableResult
    func saveDraft(_ composer: ComposerModel) async throws -> Draft {
        let draft = try await engine.dispatchDraft(KeyAction(["kind": "save_draft", "draft_id": composer.draft.id, "patch": composer.patch]))
        composer.draft = draft
        await refreshOutbox()
        return draft
    }

    /// Esc: dismisses the snippet picker first, then closes, and the route
    /// setter autosaves like closeComposer in App.svelte.
    func escapeComposer() {
        guard let composer else { return }
        if composer.snippet != nil {
            composer.snippet = nil
            return
        }
        leave(composer)
    }

    /// Saves a composer that lost the screen. Saves run one at a time so a
    /// quit can wait for all of them.
    func autosave(_ composer: ComposerModel) {
        guard !composer.finished else { return }
        let previous = draftSaves
        pendingDraftSaves += 1
        draftSaves = Task {
            await previous?.value
            do { try await saveDraft(composer) } catch { show(error) }
            pendingDraftSaves -= 1
        }
    }

    var hasUnsavedDraft: Bool { composer != nil || pendingDraftSaves > 0 }

    /// AppKit refuses to quit while the composer sheet is up, so quitting
    /// closes it first; the route change autosaves, and
    /// applicationShouldTerminate waits for the save.
    func quit() {
        composer?.snippet = nil
        escapeComposer()
        Task {
            var waits = 0
            while waits < 20, NSApp.windows.contains(where: { $0.attachedSheet != nil }) {
                waits += 1
                try? await Task.sleep(for: .milliseconds(50))
            }
            NSApp.terminate(nil)
        }
    }

    /// Quit: saves the open composer and waits for every autosave.
    func saveDraftsBeforeQuit() async {
        if let composer { autosave(composer) }
        await draftSaves?.value
    }

    private func leave(_ composer: ComposerModel, finished: Bool = false) {
        guard self.composer === composer else { return }
        composer.finished = finished
        route = composer.background
        Task {
            switch route {
            case .scheduled: await reloadScheduled()
            case .list: await refreshList()
            case .thread: await reloadOpenThread()
            default: break
            }
        }
    }

    func send(_ composer: ComposerModel) async throws {
        let draft = try await saveDraft(composer)
        composer.finished = true
        do {
            _ = try await engine.dispatch(KeyAction(["kind": "send_draft", "draft_id": draft.id]))
        } catch {
            composer.finished = false
            throw error
        }
        leave(composer, finished: true)
        show("Sending…")
        let report = try await engine.flushOutbox()
        await refreshOutbox()
        await reloadOpenThread()
        if report.failed > 0 {
            show(report.lastError ?? "\(report.failed) outbox operation(s) failed", isError: true)
        } else {
            show("Sent")
        }
    }

    func sendLater(_ composer: ComposerModel, at date: Date) async {
        do {
            let draft = try await saveDraft(composer)
            _ = try await engine.dispatch(KeyAction(["kind": "send_later", "draft_id": draft.id, "at": date.formatted(.iso8601)]))
            leave(composer, finished: true)
            show("Scheduled for \(date.formatted(date: .abbreviated, time: .shortened))")
        } catch {
            show(error)
        }
    }

    func discard(_ composer: ComposerModel) async {
        do {
            _ = try await engine.dispatch(KeyAction(["kind": "discard_draft", "draft_id": composer.draft.id]))
            leave(composer, finished: true)
            show("Draft discarded")
            _ = try? await engine.flushOutbox()
            await refreshOutbox()
        } catch {
            show(error)
        }
    }

    func attach(_ urls: [URL], to composer: ComposerModel) async {
        for url in urls {
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            do {
                composer.attachments.append(try await engine.stageAttachment(file: url))
            } catch {
                show("Couldn't attach \(url.lastPathComponent): \(error.localizedDescription)", isError: true)
            }
        }
    }

    // MARK: Scheduled

    func openScheduled() async throws {
        route = .scheduled(ScheduledState(sends: try await engine.listScheduled()))
    }

    func reloadScheduled() async {
        guard let sends = try? await engine.listScheduled(), var state = scheduled else { return }
        let index = state.sends.firstIndex { $0.id == state.selection } ?? 0
        state.sends = sends
        if !sends.contains(where: { $0.id == state.selection }) {
            state.selection = sends.isEmpty ? nil : sends[min(index, sends.count - 1)].id
        }
        scheduled = state
    }

    /// Keys in the Scheduled view, like App.svelte: Return opens the draft,
    /// `#` cancels the send. Other mail actions have no thread here and are
    /// swallowed so they never reach the list behind.
    func runInScheduled(_ action: KeyAction) async throws -> Bool {
        guard var state = scheduled else { return false }
        switch action.kind {
        case "select_next", "select_prev":
            guard !state.sends.isEmpty else { return true }
            let current = state.sends.firstIndex { $0.id == state.selected?.id } ?? 0
            let next = min(max(current + (action.kind == "select_next" ? 1 : -1), 0), state.sends.count - 1)
            state.selection = state.sends[next].id
            scheduled = state
        case "open_thread":
            if let send = state.selected { try await openScheduledDraft(send) }
        case "trash":
            if let send = state.selected { try await cancelScheduled(send) }
        case "archive", "mute", "star", "mark_read", "add_label", "remove_label", "snooze", "unsubscribe", "load_older",
            "reply", "forward", "inbox_split_important", "inbox_split_other", "inbox_split_updates", "inbox_split_newsletters":
            break
        default:
            return false
        }
        return true
    }

    func openScheduledDraft(_ send: ScheduledSend) async throws {
        scheduled?.selection = send.id
        openComposer(try await engine.openDraft(send.draftId))
    }

    func cancelScheduled(_ send: ScheduledSend) async throws {
        _ = try await engine.dispatch(KeyAction(["kind": "cancel_send_later", "send_later_id": send.sendLaterId]))
        await reloadScheduled()
        show("Scheduled send cancelled")
    }

    // MARK: Debug server

    var composerDebugState: Any {
        guard let composer else { return NSNull() }
        return [
            "title": composer.title,
            "draft_id": composer.draft.id,
            "account": composer.draft.accountId,
            "to": composer.to, "cc": composer.cc, "bcc": composer.bcc,
            "subject": composer.subject, "body": composer.body,
            "attachments": composer.attachments.map(\.filename),
            "snippet_query": composer.snippet?.query ?? NSNull(),
            "snippet_matches": composer.snippetMatches.map(\.name),
            "snippet_selection": composer.snippetSelection,
            "send_later_presets": composer.presets.map(\.label),
        ] as [String: Any]
    }

    var scheduledDebugState: Any {
        guard let scheduled else { return NSNull() }
        return [
            "count": scheduled.sends.count,
            "selected": scheduled.selected?.id ?? NSNull(),
            "sends": scheduled.sends.map { ["id": $0.id, "subject": $0.subject, "send_at": $0.sendAt.formatted(.iso8601)] },
        ] as [String: Any]
    }

    /// Debug-only composer actions: `compose_fields` sets any of to, cc, bcc,
    /// subject, body; `attach` stages a file by path; `close_composer` is Esc;
    /// `discard_draft` discards the open draft or `draft_id`.
    func debugCompose(_ name: String, _ object: [String: Any]) async throws {
        switch name {
        case "discard_draft" where object["draft_id"] is String:
            _ = try await engine.dispatch(KeyAction(["kind": "discard_draft", "draft_id": object["draft_id"]!]))
            _ = try? await engine.flushOutbox()
            await refreshOutbox()
            return
        default: break
        }
        guard let composer else { throw EngineFailure(message: "no composer open") }
        switch name {
        case "compose_fields":
            if let value = object["to"] as? String { composer.to = value }
            if let value = object["cc"] as? String { composer.cc = value }
            if let value = object["bcc"] as? String { composer.bcc = value }
            if let value = object["subject"] as? String { composer.subject = value }
            if let value = object["body"] as? String { composer.body = value }
            // Typing, caret moves, and Return in the body, through the text system.
            if let view = composer.bodyView {
                if let text = object["type"] as? String { view.insertText(text, replacementRange: view.selectedRange()) }
                if let caret = object["caret"] as? Int { view.setSelectedRange(NSRange(location: caret, length: 0)) }
                if object["return"] as? Bool == true { view.doCommand(by: #selector(NSResponder.insertNewline(_:))) }
            }
        case "attach":
            guard let path = object["path"] as? String else { throw EngineFailure(message: "attach needs path") }
            await attach([URL(fileURLWithPath: path)], to: composer)
        case "close_composer": escapeComposer()
        case "discard_draft": await discard(composer)
        default: throw EngineFailure(message: "unknown composer action \(name)")
        }
    }
}

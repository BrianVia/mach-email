import Foundation
import MachBridge

/// Typed face of the Rust engine (`MachEngine` from crates/mach-ffi). Every
/// method decodes the bridge's JSON into the models in Models.swift.
final class Engine: Sendable {
    private let core: MachEngine
    /// `mail-synced` and `sync-status`, in the order the engine emits them.
    let events: AsyncStream<EngineEvent>

    private final class Listener: EngineListener {
        let deliver: @Sendable (EngineEvent) -> Void
        init(deliver: @escaping @Sendable (EngineEvent) -> Void) { self.deliver = deliver }

        func onEvent(name: String, payloadJson: String) {
            do {
                switch name {
                case "mail-synced": deliver(.mailSynced(try Bridge.decode(MailSyncedPayload.self, payloadJson)))
                case "sync-status": deliver(.syncStatus(try Bridge.decode(SyncStatusPayload.self, payloadJson)))
                default: NSLog("mach: ignoring engine event \(name)")
                }
            } catch {
                NSLog("mach: undecodable \(name) event: \(error)")
            }
        }
    }

    init() throws {
        let (events, continuation) = AsyncStream.makeStream(of: EngineEvent.self)
        self.events = events
        core = try MachEngine.start(listener: Listener { continuation.yield($0) })
    }

    func accountStatus() throws -> AccountStatus { try Bridge.decode(AccountStatus.self, core.accountStatus()) }
    func settings() throws -> Settings { try Bridge.decode(Settings.self, core.settings()) }

    func listLabels() async throws -> [MailLabel] {
        try Bridge.decode(Out<[MailLabel]>.self, await core.listLabels()).get()
    }

    func listThreads(label: String, limit: Int) async throws -> [ThreadSummary] {
        try Bridge.decode(Out<[ThreadSummary]>.self, await core.listThreads(labelId: label, limit: UInt32(limit))).get()
    }

    func openThread(_ id: String, fetch: Bool) async throws -> ThreadDetail {
        try Bridge.decode(Out<ThreadDetail>.self, await core.openThread(threadId: id, fetch: fetch)).get()
    }

    func refetchThread(_ id: String) async throws -> ThreadDetail {
        try Bridge.decode(Out<ThreadDetail>.self, await core.refetchThread(threadId: id)).get()
    }

    func loadOlder(label: String, before: Date) async throws -> LoadOlderStats {
        let beforeMs = Int64(before.timeIntervalSince1970 * 1000)
        return try Bridge.decode(Out<LoadOlderStats>.self, await core.loadOlder(label: label, beforeMs: beforeMs)).get()
    }

    func dispatch(_ action: KeyAction) async throws -> ActionOutcome {
        try Bridge.decode(ActionOutcome.self, await core.dispatchAction(actionJson: String(decoding: action.json, as: UTF8.self)))
    }

    func syncNow() async throws -> SyncNowReport {
        try Bridge.decode(Out<SyncNowReport>.self, await core.syncNow()).get()
    }

    func outboxSummary() async throws -> OutboxSummary {
        try Bridge.decode(Out<OutboxSummary>.self, await core.outboxSummary()).get()
    }

    func retryOutbox() async throws -> Int {
        try Bridge.decode(Out<Int>.self, await core.retryOutbox()).get()
    }

    func flushOutbox() async throws -> FlushReport {
        try Bridge.decode(Out<FlushReport>.self, await core.flushOutbox()).get()
    }

    func attachment(account: String, messageId: String, attachmentId: String) async throws -> (data: Data, mime: String) {
        let result = try await core.attachment(account: account, messageId: messageId, attachmentId: attachmentId)
        return (result.bytes, result.mime)
    }

    /// Returns the saved file's path in ~/Downloads.
    func saveAttachment(account: String, messageId: String, attachment: AttachmentRef) async throws -> String {
        let json = try await core.saveAttachment(
            account: account, messageId: messageId, attachmentId: attachment.attachmentId, filename: attachment.filename)
        return try Bridge.decode(String.self, json)
    }

    func search(_ query: String, limit: Int) async throws -> [ThreadSummary] {
        try Bridge.decode(Out<[ThreadSummary]>.self, await core.search(query: query, limit: UInt32(limit))).get()
    }

    func listActivity(limit: Int) async throws -> [ActivityEntry] {
        try Bridge.decode(Out<[ActivityEntry]>.self, await core.listActivity(sinceMs: 0, limit: UInt32(limit))).get()
    }

    /// Opens the browser for Google sign-in; returns the added account's email.
    func addAccount(loginHint: String? = nil) async throws -> String {
        try Bridge.decode(String.self, await core.addAccount(loginHint: loginHint))
    }

    /// Every binding in `mode`, sorted by chord, for the command palette.
    func keymapBindings(mode: KeyMode) -> [KeyBinding] {
        (try? Bridge.decode([KeyBinding].self, core.keymapBindings(mode: mode.rawValue))) ?? []
    }

    func resolveKey(mode: KeyMode, chord: String, context: KeyContext) -> KeyResolution {
        let contextJson = (try? JSONEncoder().encode(context)).map { String(decoding: $0, as: UTF8.self) } ?? "{}"
        let json = core.resolveKey(mode: mode.rawValue, chordSequence: chord, contextJson: contextJson)
        guard let object = try? JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any] else { return .unbound }
        switch object["kind"] as? String {
        case "action":
            guard let action = object["action"] as? [String: Any], let parsed = try? KeyAction(action) else { return .unbound }
            return .action(parsed)
        case "prefix":
            let continuations = (object["continuations"] as? [[String: Any]] ?? []).compactMap { item in
                (item["next"] as? String).map { Continuation(next: $0, actionName: item["action_name"] as? String ?? "") }
            }
            return .prefix(continuations)
        default:
            return .unbound
        }
    }

    func log(_ level: String, _ message: String) {
        _ = try? core.frontendLog(level: level, message: message)
    }
}

/// Keymap sections in config/default-keymap.toml.
enum KeyMode: String, Sendable {
    case normal, reading, composing
}

struct KeyContext: Encodable, Sendable {
    var selection: [String] = []
    var currentThread: String?
    var currentMessage: String?

    enum CodingKeys: String, CodingKey {
        case selection
        case currentThread = "current_thread"
        case currentMessage = "current_message"
    }
}

import Foundation

// Codable mirrors of the bridge's JSON. Every bridge
// payload decodes through `Bridge.decoder`; fields the native UI does not use
// yet (calendar, drafts) are left out and ignored by the decoder.

struct ThreadSummary: Codable, Sendable, Identifiable, Hashable {
    var accountId: String
    var id: String
    var subject: String
    var snippet: String
    var participants: [String]
    var lastMessageAt: Date
    var messageCount: Int
    var unread: Bool
    var starred: Bool
    var labelIds: [String]
}

struct MailLabel: Codable, Sendable, Identifiable, Hashable {
    var accountId: String
    var id: String
    var name: String
    var system: Bool
    var unreadCount: Int?
}

struct InlineImageRef: Codable, Sendable, Hashable {
    var contentId: String
    var attachmentId: String
}

struct AttachmentRef: Codable, Sendable, Hashable, Identifiable {
    var attachmentId: String
    var filename: String
    var mimeType: String
    var size: Int64
    var id: String { attachmentId }
    var isImage: Bool { mimeType.hasPrefix("image/") }
}

struct MessageHeaders: Codable, Sendable, Hashable {
    var listUnsubscribe: String?
}

struct Message: Codable, Sendable, Identifiable, Hashable {
    var accountId: String
    var id: String
    var threadId: String
    var from: String
    var to: [String]
    var cc: [String]
    var subject: String
    var snippet: String
    var internalDate: Date
    var bodyPlain: String?
    var bodyHtml: String?
    var calendar: CalendarInvite?
    var headers: MessageHeaders?
    var labelIds: [String]?
    var fetchedFull: Bool
    var inlineImages: [InlineImageRef]
    var attachments: [AttachmentRef]
}

struct CalendarInvite: Codable, Sendable, Hashable {
    var summary: String
    var startsAt: Date
    var endsAt: Date
    var location: String?
    var organizer: String
    /// needs_action, accepted, declined, or tentative.
    var myStatus: String?
}

/// One `List-Unsubscribe` method, as the engine's `unsubscribe` action reports it.
enum UnsubscribeTarget: Decodable, Sendable, Hashable {
    case https(url: String, oneClick: Bool)
    case mailto(to: String, subject: String)

    private enum Key: String, CodingKey { case kind, url, oneClick, to, subject }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        if try container.decode(String.self, forKey: .kind) == "mailto" {
            self = .mailto(to: try container.decode(String.self, forKey: .to), subject: try container.decode(String.self, forKey: .subject))
        } else {
            self = .https(url: try container.decode(String.self, forKey: .url), oneClick: try container.decode(Bool.self, forKey: .oneClick))
        }
    }
}

struct ThreadDetail: Codable, Sendable {
    var thread: ThreadSummary
    var messages: [Message]
    var bodyFetchError: String?
}

struct AccountStatus: Codable, Sendable {
    var email: String
    var accounts: [String]
    var accountLabels: [String: String]
    var online: Bool
    var needsReauth: [String]
}

struct OutboxSummary: Codable, Sendable, Equatable {
    var pending: Int
    var failed: Int
    var lastError: String?
}

struct SyncNowReport: Codable, Sendable {
    var synced: Int
    var failed: Int
    var lastError: String?
}

struct FlushReport: Codable, Sendable {
    var processed: Int
    var failed: Int
    var lastError: String?
}

struct LoadOlderStats: Codable, Sendable {
    var fetched: Int
}

struct ActionOutcome: Codable, Sendable {
    var actionName: String
    var changedThreads: [String]
    var message: String
}

/// settings.toml. Each field decodes on its own, so one bad value falls back
/// to that field's default instead of dropping `block_remote_images`.
struct Settings: Decodable, Sendable {
    enum AfterArchive: String, Codable, Sendable { case next, list }
    var afterArchive: AfterArchive?
    var blockRemoteImages: Bool?
    var accountLabels: [String: String]?
    /// Why settings.toml was not fully read, for a notice.
    var problem: String?

    private enum Key: String, CodingKey { case afterArchive, blockRemoteImages, accountLabels, settingsError }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        var invalid: [String] = []
        func field<T: Decodable>(_ key: Key, _ name: String) -> T? {
            do { return try container.decodeIfPresent(T.self, forKey: key) } catch {
                invalid.append(name)
                return nil
            }
        }
        afterArchive = field(.afterArchive, "after_archive")
        blockRemoteImages = field(.blockRemoteImages, "block_remote_images")
        accountLabels = field(.accountLabels, "accounts")
        if let error = try? container.decodeIfPresent(String.self, forKey: .settingsError) {
            problem = "settings.toml couldn't be read, using defaults: \(error)"
        } else if !invalid.isEmpty {
            problem = "settings.toml: ignored invalid \(invalid.joined(separator: ", ")), using the default"
        }
    }
}

struct MailSyncedPayload: Codable, Sendable {
    var account: String
    var newThreads: [ThreadSummary]
}

struct SyncStatusPayload: Codable, Sendable {
    var account: String
    var ok: Bool
    var error: String?
}

struct ActivityEntry: Codable, Sendable, Identifiable, Hashable {
    var id: Int64
    var accountId: String
    var at: Date
    var kind: String
    var threadIds: [String]
    var summary: String
    var state: String
    var undone: Bool

    /// Only label changes and trash can be reversed, as the engine records them.
    var undoable: Bool { !undone && (kind == "modify_labels" || kind == "trash") }
}

struct KeyBinding: Codable, Sendable, Hashable {
    var chord: String
    var name: String
}

enum EngineEvent: Sendable {
    case mailSynced(MailSyncedPayload)
    case syncStatus(SyncStatusPayload)
}

/// The bridge's `{"ok": T} | {"err": "..."}` envelope.
enum Out<T: Decodable & Sendable>: Decodable, Sendable {
    case ok(T)
    case err(String)

    private enum Key: String, CodingKey { case ok, err }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: Key.self)
        if let message = try container.decodeIfPresent(String.self, forKey: .err) {
            self = .err(message)
        } else {
            self = .ok(try container.decode(T.self, forKey: .ok))
        }
    }

    func get() throws -> T {
        switch self {
        case .ok(let value): value
        case .err(let message): throw EngineFailure(message: message)
        }
    }
}

struct EngineFailure: Error, CustomStringConvertible {
    var message: String
    var description: String { message }
}

/// A keymap action as `resolveKey` returns it. `json` goes back to
/// `dispatchAction` untouched; the typed fields are what the app reads itself.
struct KeyAction: Sendable {
    struct Fields: Decodable, Sendable {
        var kind: String
        var id: String?
        var labelId: String?
        var threadIds: [String]?
        var starred: Bool?
        var read: Bool?
        var messageId: String?
        var response: String?
        var outboxId: Int64?
    }

    let json: Data
    let fields: Fields
    var kind: String { fields.kind }

    init(json: Data) throws {
        self.json = json
        fields = try Bridge.decoder.decode(Fields.self, from: json)
    }

    init(_ object: [String: Any]) throws {
        try self.init(json: JSONSerialization.data(withJSONObject: object))
    }
}

struct Continuation: Sendable, Hashable {
    var next: String
    var actionName: String
}

enum KeyResolution: Sendable {
    case action(KeyAction)
    case prefix([Continuation])
    case unbound
}

enum Bridge {
    static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let raw = try container.decode(String.self)
            guard let date = parseDate(raw) else {
                throw DecodingError.dataCorruptedError(in: container, debugDescription: "bad RFC 3339 date \(raw)")
            }
            return date
        }
        return decoder
    }()

    // ISO8601DateFormatter is documented thread-safe.
    nonisolated(unsafe) private static let wholeFormatter = ISO8601DateFormatter()
    nonisolated(unsafe) private static let fractionalFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    /// chrono writes RFC 3339 with up to nine fractional digits; Foundation
    /// parses at most three, so the rest are dropped.
    static func parseDate(_ raw: String) -> Date? {
        var text = Substring(raw)
        if let dot = text.firstIndex(of: ".") {
            let end = text[dot...].dropFirst().firstIndex { !$0.isNumber } ?? text.endIndex
            let digits = text[text.index(after: dot)..<end]
            if digits.count > 3 { text = text[..<digits.index(digits.startIndex, offsetBy: 3)] + text[end...] }
        }
        return (text.contains(".") ? fractionalFormatter : wholeFormatter).date(from: String(text))
    }

    static func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
        try decoder.decode(type, from: Data(json.utf8))
    }
}

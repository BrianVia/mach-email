import Foundation

// Field names mirror crates/mach-desktop/ui/src/lib/ipc.ts so the bridge's JSON
// decodes with `.convertFromSnakeCase`.
struct AccountStatus: Codable, Sendable {
    var email: String
    var accounts: [String]
    var online: Bool
    var needsReauth: [String]
}

struct MailLabel: Codable, Sendable, Identifiable, Hashable {
    var accountId: String
    var id: String
    var name: String
    var system: Bool
    var unreadCount: Int?
}

struct ThreadSummary: Codable, Sendable, Identifiable, Hashable {
    var accountId: String
    var id: String
    var subject: String
    var snippet: String
    var participants: [String]
    var lastMessageAt: Date
    var unread: Bool
    var starred: Bool
}

/// What the Rust bridge (mach-ffi) will provide. One method per Tauri command.
protocol MailEngine: Sendable {
    func accountStatus() async throws -> AccountStatus
    func listLabels() async throws -> [MailLabel]
    func listThreads(label: String) async throws -> [ThreadSummary]
}

struct PreviewEngine: MailEngine {
    private static let account = "preview@example.com"

    func accountStatus() async throws -> AccountStatus {
        AccountStatus(email: Self.account, accounts: [Self.account], online: true, needsReauth: [])
    }

    func listLabels() async throws -> [MailLabel] {
        [("INBOX", "Inbox", 3), ("STARRED", "Starred", nil), ("SENT", "Sent", nil), ("Label_1", "Receipts", 1)]
            .map { MailLabel(accountId: Self.account, id: $0.0, name: $0.1, system: $0.0 == $0.0.uppercased(), unreadCount: $0.2) }
    }

    func listThreads(label: String) async throws -> [ThreadSummary] {
        let rows: [(String, String, String)] = [
            ("Ada Lovelace", "Notes on the analytical engine", "I have attached the translation with my notes."),
            ("Grace Hopper", "Compiler review", "Found a moth in relay 70. Taping it to the log."),
            ("Alan Turing", "Lunch on Thursday?", "Also, can machines think? Discuss over lunch."),
            ("Linus Torvalds", "Re: patch v3", "This is fine, but the commit message needs work."),
            ("Margaret Hamilton", "Priority displays", "The 1202 alarm was the software doing its job."),
            ("Dennis Ritchie", "K&R second edition", "Draft of chapter 5 attached for review."),
        ]
        let now = Date()
        return rows.enumerated().map { i, row in
            ThreadSummary(
                accountId: Self.account, id: "\(label.lowercased())-\(i)", subject: row.1, snippet: row.2,
                participants: [row.0], lastMessageAt: now.addingTimeInterval(Double(-i) * 3600),
                unread: i < 3, starred: i == 1)
        }
    }
}

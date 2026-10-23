import Foundation
import Testing

@testable import Mach

private func thread(_ id: String, subject: String? = nil) -> ThreadSummary {
    ThreadSummary(
        accountId: "me@example.com", id: id, subject: subject ?? "Subject \(id)", snippet: "Snippet \(id)",
        participants: ["Sender \(id) <\(id)@example.com>"], lastMessageAt: .now, messageCount: 1,
        unread: true, starred: false, labelIds: ["INBOX"])
}

@Test(arguments: [
    (["INBOX", "IMPORTANT"], Split.important),
    (["IMPORTANT", "CATEGORY_PROMOTIONS"], .newsletters),
    (["CATEGORY_UPDATES"], .updates),
    (["CATEGORY_UPDATES", "CATEGORY_PROMOTIONS"], .updates),
    (["CATEGORY_SOCIAL"], .other),
])
func splitClassifiesLabels(labels: [String], split: Split) {
    #expect(Split.of(labels) == split)
}

@Test func snippetTokenStartsAtLineStartOrAfterWhitespace() {
    #expect(SnippetToken.detect(in: ";tha", cursor: 4) == SnippetToken(start: 0, query: "tha"))
    #expect(SnippetToken.detect(in: "Hello\n;eta", cursor: 10) == SnippetToken(start: 6, query: "eta"))
    #expect(SnippetToken.detect(in: "Hello ;thanks", cursor: 13) == SnippetToken(start: 6, query: "thanks"))
    #expect(SnippetToken.detect(in: "word;tha", cursor: 8) == nil)
    #expect(SnippetToken.detect(in: ";two words", cursor: 10) == nil)
}

@Test func notificationsBatchThreeAndCollapseTheRest() {
    let threads = [thread("1"), thread("2", subject: ""), thread("3"), thread("4"), thread("5")]
    let batch = NewMailNotification.batch(threads)
    #expect(batch.map(\.title) == ["Sender 1", "Sender 2", "Sender 3", "2 more new messages"])
    #expect(batch.map(\.body) == ["Subject 1", "Snippet 2", "Subject 3", nil])
    #expect(batch.map(\.thread.id) == ["1", "2", "3", "1"])
}

@Test func threadMarkdownKeepsThePlainBodyVerbatim() {
    let date = Calendar.current.date(from: DateComponents(year: 2026, month: 8, day: 28, hour: 14, minute: 5))!
    let message = Message(
        accountId: "me@example.com", id: "m1", threadId: "thread-1", from: "Ada Lovelace <ada@example.com>",
        to: ["me@example.com", "team@example.com"], cc: [], subject: "Status", snippet: "", internalDate: date,
        bodyPlain: "First line\n\nSecond line", bodyHtml: nil, calendar: nil, headers: nil, labelIds: nil,
        fetchedFull: true, inlineImages: [], attachments: [])
    #expect(
        ThreadMarkdown.render(thread("thread-1", subject: "Status"), [message]) == """
            # Status

            > mach thread id: `thread-1` (account me@example.com) — an agent can open it with the mach MCP `open_thread` action.

            ## Ada Lovelace — 2026-08-28 14:05
            From: Ada Lovelace <ada@example.com> | To: me@example.com, team@example.com

            First line

            Second line
            """)
}

@Test func remoteImageCountSkipsCidImages() {
    let html = #"<img src="https://tracker.example/pixel.gif"><img src='//cdn.example/a.png'><img src="cid:logo">"#
    #expect(EmailHTML.remoteImageCount(html) == 2)
}

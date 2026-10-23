import Foundation

/// A thread as Markdown for pasting into an agent (`ctrl+shift+c`).
enum ThreadMarkdown {
    static func render(_ thread: ThreadSummary, _ messages: [Message]) -> String {
        let heading = thread.subject.isEmpty ? "(no subject)" : thread.subject
        let intro = "> mach thread id: `\(thread.id)` (account \(thread.accountId)) — an agent can open it with the mach MCP `open_thread` action."
        let sections = messages.map { message in
            let body = message.bodyPlain ?? message.bodyHtml.map(markdown(fromHTML:)) ?? message.snippet
            return "## \(senderName(message.from)) — \(dateFormatter.string(from: message.internalDate))\n"
                + "From: \(message.from) | To: \(message.to.joined(separator: ", "))\n\n\(body)"
        }
        return "# \(heading)\n\n\(intro)\n\n\(sections.joined(separator: "\n\n---\n\n"))"
    }

    private static func senderName(_ from: String) -> String {
        from.firstMatch(of: /^\s*"?([^"<]+?)"?\s*<.+>$/).map { String($0.1).trimmingCharacters(in: .whitespaces) } ?? from
    }

    private static let dateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd HH:mm"
        return formatter
    }()

    static func markdown(fromHTML html: String) -> String {
        guard let document = try? XMLDocument(xmlString: html, options: [.documentTidyHTML]),
            let root = document.rootElement()
        else { return html }
        let body = root.elements(forName: "body").first ?? root
        return walk(body).replacing(/\n{3,}/, with: "\n\n").trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func walk(_ node: XMLNode) -> String {
        if node.kind == .text { return (node.stringValue ?? "").replacing(/\s+/, with: " ") }
        guard let element = node as? XMLElement, let tag = element.localName?.lowercased() else { return "" }
        if ["style", "script", "head"].contains(tag) { return "" }
        let text = (element.children ?? []).map(walk).joined()
        switch tag {
        case "a":
            guard let href = element.attribute(forName: "href")?.stringValue else { return text }
            let label = text.trimmingCharacters(in: .whitespacesAndNewlines)
            return label == href ? href : "[\(label)](\(href))"
        case "strong", "b": return "**\(text)**"
        case "em", "i": return "*\(text)*"
        case "h1", "h2", "h3", "h4":
            return "\(String(repeating: "#", count: Int(String(tag.last!))!)) \(text.trimmingCharacters(in: .whitespacesAndNewlines))\n\n"
        case "li":
            let parent = element.parent as? XMLElement
            let siblings = (parent?.children ?? []).filter { ($0 as? XMLElement)?.localName?.lowercased() == "li" }
            let marker = parent?.localName?.lowercased() == "ol" ? "\((siblings.firstIndex { $0 === element } ?? 0) + 1)." : "-"
            return "\(marker) \(text.trimmingCharacters(in: .whitespacesAndNewlines))\n"
        case "blockquote":
            return text.trimmingCharacters(in: .whitespacesAndNewlines).split(separator: "\n", omittingEmptySubsequences: false)
                .map { "> \($0)" }.joined(separator: "\n") + "\n\n"
        case "br", "tr": return "\(text)\n"
        case "p", "div": return "\(text)\n\n"
        default: return text
        }
    }
}

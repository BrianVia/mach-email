import SwiftUI

struct ThreadList: View {
    @Bindable var model: AppModel

    var body: some View {
        let threads = model.visibleThreads
        let multipleAccounts = Set(threads.map(\.accountId)).count > 1
        ScrollViewReader { proxy in
            List(selection: selection) {
                ForEach(threads) { thread in
                    ThreadRow(thread: thread, accountLabel: multipleAccounts ? model.accountLabel(thread.accountId) : nil)
                        .tag(thread.id)
                        .id(thread.id)
                        .contentShape(Rectangle())
                        // A click opens the thread; j/k and arrow keys only move the selection.
                        .simultaneousGesture(TapGesture().onEnded { open(thread.id) })
                }
                if !threads.isEmpty && model.list.threads.count >= model.list.limit {
                    Button("Load Older") { Task { do { try await model.loadOlder() } catch { model.show(error) } } }
                        .buttonStyle(.link)
                        .frame(maxWidth: .infinity)
                        .selectionDisabled()
                }
            }
            .contextMenu(forSelectionType: String.self, menu: { _ in }) { ids in
                if let id = ids.first { open(id) }
            }
            .overlay {
                if threads.isEmpty {
                    ContentUnavailableView("Nothing Here", systemImage: "tray", description: Text("No threads in this view."))
                }
            }
            .safeAreaInset(edge: .top, spacing: 0) {
                if model.list.label == "INBOX" { splitPicker }
            }
            .onChange(of: model.list.selection) { _, id in
                if let id { proxy.scrollTo(id) }
            }
        }
    }

    private var splitPicker: some View {
        Picker("Category", selection: Binding(get: { model.split }, set: { model.selectSplit($0) })) {
            ForEach(Split.allCases) { split in
                let unread = model.unreadCount(in: split)
                Text(unread > 0 ? "\(split.title) \(unread)" : split.title).tag(split)
            }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(.bar)
    }

    private var selection: Binding<String?> {
        Binding { model.list.selection } set: { id in
            if let id { model.list.selection = id }
        }
    }

    private func open(_ id: String) {
        guard model.reader?.thread.id != id else { return }
        Task { do { try await model.openThread(id) } catch { model.show(error) } }
    }
}

struct ThreadRow: View {
    let thread: ThreadSummary
    let accountLabel: String?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Circle()
                .fill(thread.unread ? Color.accentColor : .clear)
                .frame(width: 8, height: 8)
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(Sender.name(thread.participants.first ?? "(no sender)"))
                        .fontWeight(thread.unread ? .semibold : .regular)
                        .lineLimit(1)
                    if thread.messageCount > 1 {
                        Text("\(thread.messageCount)").font(.caption).foregroundStyle(.secondary)
                    }
                    if let accountLabel {
                        Text(accountLabel)
                            .font(.caption2)
                            .lineLimit(1)
                            .padding(.horizontal, 5)
                            .padding(.vertical, 1)
                            .foregroundStyle(Sender.color(thread.accountId))
                            .background(Sender.color(thread.accountId).opacity(0.15), in: Capsule())
                    }
                    Spacer(minLength: 4)
                    if thread.starred {
                        Image(systemName: "star.fill").font(.caption).foregroundStyle(.yellow)
                    }
                    Text(RelativeDate.short(thread.lastMessageAt))
                        .font(.caption)
                        .foregroundStyle(thread.unread ? .primary : .secondary)
                }
                Text(thread.subject.isEmpty ? "(no subject)" : thread.subject)
                    .fontWeight(thread.unread ? .medium : .regular)
                    .lineLimit(1)
                Text(thread.snippet.htmlUnescaped)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .padding(.vertical, 4)
    }
}

/// Display helpers for "Name <email>" strings, shared by the list and reader.
enum Sender {
    static func name(_ raw: String) -> String {
        if let match = raw.firstMatch(of: /^\s*"?([^"<]+?)"?\s*<.+>$/) { return String(match.1).trimmingCharacters(in: .whitespaces) }
        if let match = raw.firstMatch(of: /^[\w.+-]+@([\w.-]+)$/) { return String(match.1) }
        if let match = raw.firstMatch(of: /^\s*<([^>]+)>\s*$/) { return String(match.1) }
        return raw
    }

    static func email(_ raw: String) -> String {
        if let match = raw.firstMatch(of: /<([^>]+)>/) { return String(match.1) }
        return raw.firstMatch(of: /[^\s<>@]+@[^\s<>@]+/).map { String($0.0) } ?? ""
    }

    static func initials(_ raw: String) -> String {
        // Quotes and other punctuation never make an initial: `"Clark, Ada"` is CA, not "C.
        let name = raw.replacing(/<[^>]+>/, with: "").filter { !"\"'“”‘’()[]".contains($0) }.trimmingCharacters(in: .whitespaces)
        let parts = name.split(whereSeparator: { " .,_-".contains($0) })
        if parts.count >= 2, let first = parts.first?.first, let last = parts.last?.first { return "\(first)\(last)".uppercased() }
        if let only = parts.first, !name.contains("@") { return String(only.prefix(2)).uppercased() }
        if let domain = raw.firstMatch(of: /@([\w-]+)/) { return String(domain.1.prefix(2)).uppercased() }
        return "?"
    }

    /// Stable per-sender hue (FNV-1a), like ui/src/lib/avatar.ts.
    static func color(_ seed: String) -> Color {
        var hash: UInt32 = 2_166_136_261
        for byte in seed.utf8 {
            hash ^= UInt32(byte)
            hash = hash &* 16_777_619
        }
        return Color(hue: Double(hash % 360) / 360, saturation: 0.45, brightness: 0.6)
    }
}

enum RelativeDate {
    /// "3:45 PM" today, "Oct 3" this year, "Oct 3, 2024" before.
    static func short(_ date: Date, now: Date = .now) -> String {
        let calendar = Calendar.current
        if calendar.isDate(date, inSameDayAs: now) { return date.formatted(date: .omitted, time: .shortened) }
        if calendar.component(.year, from: date) == calendar.component(.year, from: now) {
            return date.formatted(.dateTime.month(.abbreviated).day())
        }
        return date.formatted(.dateTime.month(.abbreviated).day().year())
    }
}

extension String {
    /// Gmail snippets arrive HTML-escaped ("it&#39;s").
    var htmlUnescaped: String {
        guard contains("&") else { return self }
        let named = ["amp": "&", "lt": "<", "gt": ">", "quot": "\"", "apos": "'", "nbsp": " "]
        return replacing(/&(#x?[0-9a-fA-F]+|[a-z]+);/) { match in
            let entity = String(match.1)
            if let text = named[entity] { return text }
            let number = entity.hasPrefix("#x") ? UInt32(entity.dropFirst(2), radix: 16) : entity.hasPrefix("#") ? UInt32(entity.dropFirst()) : nil
            return number.flatMap(Unicode.Scalar.init).map { String(Character($0)) } ?? String(match.0)
        }
    }
}

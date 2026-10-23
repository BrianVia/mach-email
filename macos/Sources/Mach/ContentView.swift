import SwiftUI

struct ContentView: View {
    @Bindable var model: AppModel

    var body: some View {
        NavigationSplitView {
            List(model.labels, selection: labelSelection) { label in
                HStack {
                    Text(label.name)
                    Spacer()
                    if let unread = label.unreadCount { Text("\(unread)").foregroundStyle(.secondary) }
                }
            }
            .navigationSplitViewColumnWidth(min: 160, ideal: 200)
        } content: {
            List(model.threads, selection: $model.selectedThreadId) { ThreadRow(thread: $0) }
                .contextMenu(forSelectionType: String.self, menu: { _ in }) { ids in
                    if let id = ids.first { model.openThread(id) }
                }
                .navigationSplitViewColumnWidth(min: 280, ideal: 360)
        } detail: {
            if let id = model.openedThreadId {
                ThreadDetail(id: id, thread: model.openedThread)
            } else {
                ContentUnavailableView("No thread open", systemImage: "envelope")
            }
        }
        .navigationTitle(model.labels.first { $0.id == model.selectedLabelId }?.name ?? "Mach")
    }

    private var labelSelection: Binding<String?> {
        Binding(get: { model.selectedLabelId }, set: { id in Task { await model.selectLabel(id) } })
    }
}

private struct ThreadRow: View {
    let thread: ThreadSummary

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Circle().fill(thread.unread ? Color.accentColor : .clear).frame(width: 7, height: 7)
                Text(thread.participants.joined(separator: ", ")).fontWeight(thread.unread ? .semibold : .regular)
                if thread.starred { Image(systemName: "star.fill").foregroundStyle(.yellow) }
                Spacer()
                Text(thread.lastMessageAt, format: .dateTime.hour().minute()).foregroundStyle(.secondary)
            }
            Text(thread.subject).lineLimit(1)
            Text(thread.snippet).lineLimit(1).foregroundStyle(.secondary)
        }
        .padding(.vertical, 2)
    }
}

private struct ThreadDetail: View {
    let id: String
    let thread: ThreadSummary?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(thread?.subject ?? "Thread \(id)").font(.title2)
            if let thread {
                Text(thread.participants.joined(separator: ", ")).foregroundStyle(.secondary)
                Text(thread.snippet)
            }
            Spacer()
        }
        .padding()
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

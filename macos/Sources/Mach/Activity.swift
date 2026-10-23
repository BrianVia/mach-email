import SwiftUI

struct ActivityState {
    var entries: [ActivityEntry]
    var selected = 0

    var selectedEntry: ActivityEntry? { entries.indices.contains(selected) ? entries[selected] : nil }
}

extension AppModel {
    func openActivity() async throws {
        route = .activity(ActivityState(entries: try await engine.listActivity(limit: 50)))
    }

    func undoActivity(_ id: Int64) async throws {
        let outcome = try await engine.dispatch(KeyAction(["kind": "undo_activity", "outbox_id": id]))
        if !outcome.message.isEmpty { show(outcome.message) }
        let entries = try await engine.listActivity(limit: 50)
        if case .activity(var state) = route {
            state.entries = entries
            route = .activity(state)
        }
        await refreshList()
        await refreshOutbox()
    }

    /// j/k and the arrows move the selection; Return or z undoes it. nil hands
    /// the key to the keymap, so `g` chords and Esc still work.
    func handleActivityKey(_ chord: String, _ state: ActivityState) -> Bool? {
        guard chordBuffer.isEmpty else { return nil }
        switch chord {
        case "j", "down", "k", "up":
            var next = state
            next.selected = min(max(state.selected + (chord == "j" || chord == "down" ? 1 : -1), 0), max(state.entries.count - 1, 0))
            route = .activity(next)
        case "enter", "z":
            guard let entry = state.selectedEntry else { return true }
            guard entry.undoable else {
                show(entry.undone ? "Already undone" : "This change can't be undone")
                return true
            }
            Task { do { try await undoActivity(entry.id) } catch { show(error) } }
        default:
            return nil
        }
        return true
    }
}

struct ActivityView: View {
    let model: AppModel
    let state: ActivityState

    var body: some View {
        ScrollViewReader { proxy in
            List(selection: selection) {
                let subjects = Dictionary(model.list.threads.map { ($0.id, $0.subject) }, uniquingKeysWith: { first, _ in first })
                ForEach(state.entries) { entry in
                    ActivityRow(entry: entry, subject: entry.threadIds.first.flatMap { subjects[$0] },
                        accountLabel: model.accountLabel(entry.accountId)) {
                        Task { do { try await model.undoActivity(entry.id) } catch { model.show(error) } }
                    }
                    .tag(entry.id)
                    .id(entry.id)
                }
            }
            .overlay {
                if state.entries.isEmpty {
                    ContentUnavailableView("No Recent Activity", systemImage: "clock.arrow.circlepath",
                        description: Text("Archives, stars, labels, and sends appear here."))
                }
            }
            .onChange(of: state.selectedEntry?.id) { _, id in
                if let id { proxy.scrollTo(id) }
            }
        }
    }

    private var selection: Binding<Int64?> {
        Binding {
            state.selectedEntry?.id
        } set: { id in
            guard let index = state.entries.firstIndex(where: { $0.id == id }) else { return }
            var next = state
            next.selected = index
            model.route = .activity(next)
        }
    }
}

private struct ActivityRow: View {
    let entry: ActivityEntry
    /// Known only when the thread is in the loaded list.
    let subject: String?
    let accountLabel: String
    let undo: () -> Void
    @ScaledMetric private var timeWidth = 62.0
    @ScaledMetric private var statusWidth = 66.0
    @ScaledMetric private var undoWidth = 54.0

    var body: some View {
        HStack(spacing: 10) {
            Text(RelativeDate.short(entry.at))
                .monospacedDigit()
                .foregroundStyle(.secondary)
                .frame(width: timeWidth, alignment: .leading)
            VStack(alignment: .leading, spacing: 1) {
                Text(entry.summary).lineLimit(1)
                if let subject {
                    Text(subject.isEmpty ? "(no subject)" : subject).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            ActivityStatus(entry: entry).frame(width: statusWidth)
            Group {
                if entry.undoable { Button("Undo", action: undo).controlSize(.small) } else { Color.clear }
            }
            .frame(width: undoWidth)
        }
        .padding(.vertical, 3)
        .help("\(accountLabel) · \(entry.at.formatted(date: .abbreviated, time: .shortened))")
    }
}

private struct ActivityStatus: View {
    let entry: ActivityEntry

    var body: some View {
        Text(entry.undone ? "undone" : entry.state)
            .font(.caption)
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .foregroundStyle(entry.state == "failed" ? .red : .secondary)
            .background(entry.state == "failed" ? AnyShapeStyle(.red.opacity(0.12)) : AnyShapeStyle(.quaternary), in: Capsule())
    }
}

/// The detail column for the selected Activity entry: what changed, when,
/// and the subjects of the threads it touched.
struct ActivityDetail: View {
    let model: AppModel
    let entry: ActivityEntry?
    @State private var subjects: [String] = []

    var body: some View {
        if let entry {
            Form {
                Section {
                    LabeledContent("Change", value: entry.summary)
                    LabeledContent("When", value: entry.at.formatted(date: .complete, time: .shortened))
                    LabeledContent("Account", value: model.accountLabel(entry.accountId))
                    LabeledContent("Status") { ActivityStatus(entry: entry) }
                }
                if !subjects.isEmpty {
                    Section(entry.threadIds.count == 1 ? "Conversation" : "Conversations") {
                        ForEach(Array(subjects.enumerated()), id: \.offset) { _, subject in Text(subject).lineLimit(2) }
                        if entry.threadIds.count > subjects.count {
                            Text("and \(entry.threadIds.count - subjects.count) more").foregroundStyle(.secondary)
                        }
                    }
                }
                if entry.undoable {
                    Section {
                        Button("Undo This Change") {
                            Task { do { try await model.undoActivity(entry.id) } catch { model.show(error) } }
                        }
                    } footer: {
                        Text("Return or z also undoes the selected change.").foregroundStyle(.secondary)
                    }
                }
            }
            .formStyle(.grouped)
            .task(id: entry.id) {
                var found: [String] = []
                for id in entry.threadIds.prefix(5) {
                    if let thread = try? await model.engine.openThread(id, fetch: false).thread {
                        found.append(thread.subject.isEmpty ? "(no subject)" : thread.subject)
                    }
                }
                subjects = found
            }
        } else {
            ContentUnavailableView("No Change Selected", systemImage: "clock.arrow.circlepath",
                description: Text("Select a change to see what it touched."))
        }
    }
}

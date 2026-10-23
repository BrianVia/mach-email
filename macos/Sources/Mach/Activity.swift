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
                ForEach(state.entries) { entry in
                    ActivityRow(entry: entry, accountLabel: model.accountLabel(entry.accountId)) {
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
    let accountLabel: String
    let undo: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(entry.summary).lineLimit(2)
                Text("\(accountLabel) · \(entry.at.formatted(date: .abbreviated, time: .shortened))")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 8)
            Text(entry.undone ? "undone" : entry.state)
                .font(.caption)
                .padding(.horizontal, 7)
                .padding(.vertical, 2)
                .foregroundStyle(entry.state == "failed" ? .red : .secondary)
                .background(entry.state == "failed" ? AnyShapeStyle(.red.opacity(0.12)) : AnyShapeStyle(.quaternary), in: Capsule())
            if entry.undoable {
                Button("Undo", action: undo).controlSize(.small)
            }
        }
        .padding(.vertical, 4)
    }
}

import SwiftUI

struct ScheduledView: View {
    let model: AppModel
    let state: ScheduledState

    var body: some View {
        if state.sends.isEmpty {
            ContentUnavailableView("Nothing Scheduled", systemImage: "clock", description: Text("Messages you schedule with Send Later appear here."))
        } else {
            List(selection: Binding(get: { state.selected?.id }, set: { model.scheduled?.selection = $0 })) {
                ForEach(state.sends) { send in
                    ScheduledRow(send: send, accountLabel: model.accounts.count > 1 ? model.accountLabel(send.accountId) : nil)
                        .tag(send.id)
                        .contextMenu {
                            Button("Open Draft") { run { try await model.openScheduledDraft(send) } }
                            Button("Cancel Send", role: .destructive) { run { try await model.cancelScheduled(send) } }
                        }
                }
            }
            .contextMenu(forSelectionType: String.self, menu: { _ in }) { ids in
                if let send = state.sends.first(where: { ids.contains($0.id) }) { run { try await model.openScheduledDraft(send) } }
            }
        }
    }

    private func run(_ work: @escaping () async throws -> Void) {
        Task { do { try await work() } catch { model.show(error) } }
    }
}

private struct ScheduledRow: View {
    let send: ScheduledSend
    let accountLabel: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(alignment: .firstTextBaseline) {
                Text(send.to.isEmpty ? "(no recipient)" : send.to.map(Sender.name).joined(separator: ", "))
                    .fontWeight(.medium)
                    .lineLimit(1)
                if let accountLabel {
                    Text(accountLabel).font(.caption).foregroundStyle(.secondary)
                }
                Spacer(minLength: 8)
                Label(send.sendAt.formatted(.dateTime.weekday(.abbreviated).month(.abbreviated).day().hour().minute()), systemImage: "clock")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            Text(send.subject.isEmpty ? "(no subject)" : send.subject).lineLimit(1)
        }
        .padding(.vertical, 4)
    }
}

/// The detail column for the selected scheduled send.
struct ScheduledDetail: View {
    let model: AppModel
    let send: ScheduledSend?

    var body: some View {
        if let send {
            Form {
                Section {
                    LabeledContent("Sends", value: send.sendAt.formatted(date: .complete, time: .shortened))
                    LabeledContent("From", value: model.accountLabel(send.accountId))
                    LabeledContent("To", value: send.to.isEmpty ? "(no recipient)" : send.to.joined(separator: ", "))
                    LabeledContent("Subject", value: send.subject.isEmpty ? "(no subject)" : send.subject)
                }
                Section {
                    HStack {
                        Button("Open Draft") { run { try await model.openScheduledDraft(send) } }
                        Spacer()
                        Button("Cancel Send", role: .destructive) { run { try await model.cancelScheduled(send) } }
                    }
                } footer: {
                    Text("Return opens the draft. # cancels the send.").foregroundStyle(.secondary)
                }
            }
            .formStyle(.grouped)
        } else {
            ContentUnavailableView("Nothing Scheduled", systemImage: "clock",
                description: Text("Messages you schedule with Send Later appear here."))
        }
    }

    private func run(_ work: @escaping () async throws -> Void) {
        Task { do { try await work() } catch { model.show(error) } }
    }
}

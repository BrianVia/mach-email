import SwiftUI

/// The calendar invite card from Thread.svelte, with Accept / Maybe / Decline.
struct InviteCard: View {
    let model: AppModel
    let message: Message
    let invite: CalendarInvite
    let reply: InviteReply?

    var body: some View {
        HStack(alignment: .center, spacing: 16) {
            Image(systemName: "calendar")
                .font(.title2)
                .foregroundStyle(.tint)
                .frame(width: 28)
            VStack(alignment: .leading, spacing: 3) {
                Text(invite.summary).fontWeight(.semibold)
                Text(when).foregroundStyle(.secondary)
                if let location = invite.location, !location.isEmpty {
                    Text(location).foregroundStyle(.secondary)
                }
                Text("Organizer: \(invite.organizer)").foregroundStyle(.secondary)
                if let status = invite.myStatus {
                    Text("Status: \(status.replacingOccurrences(of: "_", with: " "))").foregroundStyle(.secondary)
                }
            }
            .font(.callout)
            .textSelection(.enabled)
            Spacer(minLength: 8)
            if reply == .sent {
                Label("Reply sent", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                    .font(.callout)
            } else {
                VStack(alignment: .trailing, spacing: 6) {
                    HStack(spacing: 6) {
                        respond("Accept", "accepted", help: "Accept (⇧A)")
                        respond("Maybe", "tentative", help: "Maybe (⇧T)")
                        respond("Decline", "declined", help: "Decline (⇧D)")
                    }
                    .disabled(reply == .sending)
                    if reply == .failed {
                        Text("Reply failed").font(.caption).foregroundStyle(.red)
                    }
                }
            }
        }
        .padding(14)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 10))
        .overlay { RoundedRectangle(cornerRadius: 10).strokeBorder(.separator) }
    }

    private var when: String {
        let start = invite.startsAt.formatted(.dateTime.weekday(.wide).month(.abbreviated).day().year().hour().minute())
        let end = Calendar.current.isDate(invite.startsAt, inSameDayAs: invite.endsAt)
            ? invite.endsAt.formatted(date: .omitted, time: .shortened)
            : invite.endsAt.formatted(.dateTime.month(.abbreviated).day().hour().minute())
        return "\(start) – \(end)"
    }

    private func respond(_ title: String, _ response: String, help: String) -> some View {
        Button(title) { Task { await model.respondToInvite(messageId: message.id, response: response) } }
            .help(help)
    }
}

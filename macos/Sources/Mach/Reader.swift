import SwiftUI

struct ReaderView: View {
    let model: AppModel
    let state: ReaderState
    /// Messages the user expanded or collapsed by clicking their header.
    @State private var toggled: [String: Bool] = [:]

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(state.thread.subject.isEmpty ? "(no subject)" : state.thread.subject)
                            .font(.title2.weight(.semibold))
                            .textSelection(.enabled)
                        Text("\(state.messages.count) \(state.messages.count == 1 ? "message" : "messages")")
                            .foregroundStyle(.secondary)
                    }
                    .padding(.bottom, 4)
                    ForEach(Array(state.messages.enumerated()), id: \.element.id) { index, message in
                        let selected = index == state.selectedMessage
                        MessageCard(
                            model: model, message: message, selected: selected,
                            expanded: toggled[message.id] ?? selected,
                            toggle: { toggled[message.id] = !(toggled[message.id] ?? selected) }
                        )
                        .id(message.id)
                    }
                }
                .padding(20)
                .frame(maxWidth: 860, alignment: .leading)
                .frame(maxWidth: .infinity)
            }
            .onChange(of: state.selectedMessage, initial: true) { _, index in
                toggled = [:]
                if state.messages.indices.contains(index) {
                    proxy.scrollTo(state.messages[index].id, anchor: .top)
                }
            }
        }
        .id(state.thread.id)
    }
}

private struct MessageCard: View {
    let model: AppModel
    let message: Message
    let selected: Bool
    let expanded: Bool
    let toggle: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
                .contentShape(Rectangle())
                .onTapGesture(perform: toggle)
            if expanded {
                if !message.fetchedFull {
                    HStack {
                        Label("Preview only. The full message hasn't been fetched yet.", systemImage: "exclamationmark.triangle")
                        Spacer()
                        Button("Fetch Again") { Task { await model.refetchOpenThread() } }
                    }
                    .font(.callout)
                    .padding(10)
                    .background(.yellow.opacity(0.15), in: RoundedRectangle(cornerRadius: 8))
                }
                messageBody
                if !message.attachments.isEmpty {
                    AttachmentsRow(model: model, message: message)
                }
            }
        }
        .padding(14)
        .background(.background, in: RoundedRectangle(cornerRadius: 10))
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(selected ? Color.accentColor.opacity(0.6) : Color.primary.opacity(0.08))
        }
    }

    private var header: some View {
        HStack(alignment: .top, spacing: 10) {
            Text(Sender.initials(message.from))
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: 34, height: 34)
                .background(Sender.color(message.from), in: Circle())
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(Sender.name(message.from)).fontWeight(.semibold)
                    Text(Sender.email(message.from)).font(.callout).foregroundStyle(.secondary)
                }
                .lineLimit(1)
                if expanded {
                    Text("to \(message.to.isEmpty ? "—" : message.to.joined(separator: ", "))")
                        .font(.callout).foregroundStyle(.secondary).lineLimit(2)
                } else {
                    Text(message.bodyPlain.map { String($0.prefix(200)) } ?? message.snippet)
                        .font(.callout).foregroundStyle(.secondary).lineLimit(1)
                }
            }
            Spacer(minLength: 8)
            Text(message.internalDate.formatted(.dateTime.weekday(.abbreviated).month(.abbreviated).day().hour().minute()))
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder private var messageBody: some View {
        if let html = message.bodyHtml, !html.isEmpty {
            EmailWebView(html: EmailHTML.document(for: message), engine: model.engine)
        } else {
            Text(EmailHTML.linkified(message.bodyPlain ?? message.snippet))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

private struct AttachmentsRow: View {
    let model: AppModel
    let message: Message

    var body: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 8) {
                ForEach(message.attachments) { attachment in
                    Button { save(attachment) } label: {
                        if attachment.isImage {
                            AttachmentThumbnail(engine: model.engine, message: message, attachment: attachment)
                        } else {
                            HStack(spacing: 6) {
                                Image(systemName: "paperclip")
                                Text(attachment.filename)
                                Text(ByteCountFormatter.string(fromByteCount: attachment.size, countStyle: .file)).foregroundStyle(.secondary)
                            }
                            .padding(.horizontal, 10).padding(.vertical, 6)
                            .background(.quaternary, in: RoundedRectangle(cornerRadius: 6))
                        }
                    }
                    .buttonStyle(.plain)
                    .help("Save \(attachment.filename) to Downloads")
                }
            }
        }
        .scrollIndicators(.never)
    }

    private func save(_ attachment: AttachmentRef) {
        Task {
            do {
                let path = try await model.engine.saveAttachment(account: message.accountId, messageId: message.id, attachment: attachment)
                model.show("Saved to \(path)")
            } catch {
                model.show(error)
            }
        }
    }
}

private struct AttachmentThumbnail: View {
    let engine: Engine
    let message: Message
    let attachment: AttachmentRef
    @State private var image: NSImage?

    var body: some View {
        Group {
            if let image {
                Image(nsImage: image).resizable().scaledToFill()
            } else {
                Image(systemName: "photo").foregroundStyle(.secondary)
            }
        }
        .frame(width: 96, height: 72)
        .background(.quaternary)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .task(id: attachment.attachmentId) {
            let data = try? await engine.attachment(account: message.accountId, messageId: message.id, attachmentId: attachment.attachmentId).data
            image = data.flatMap(NSImage.init(data:))
        }
    }
}

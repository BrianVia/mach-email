import SwiftUI
import UniformTypeIdentifiers

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
                        Text(counts).foregroundStyle(.secondary)
                    }
                    .padding(.bottom, 4)
                    ForEach(Array(state.messages.enumerated()), id: \.element.id) { index, message in
                        let selected = index == state.selectedMessage
                        // Older read messages start collapsed; the selected one and unread ones start open.
                        let expanded = toggled[message.id] ?? (selected || message.labelIds?.contains("UNREAD") == true)
                        MessageCard(
                            model: model, state: state, message: message, selected: selected, expanded: expanded,
                            tap: {
                                if selected || !expanded {
                                    toggled[message.id] = !expanded
                                }
                                if !selected { model.selectMessage(index) }
                            }
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
                    withAnimation(.snappy(duration: 0.2)) { proxy.scrollTo(state.messages[index].id, anchor: .top) }
                }
            }
        }
        .alert(
            unsubscribeTitle, isPresented: Binding(get: { state.unsubscribe != nil }, set: { if !$0 { model.cancelUnsubscribe() } }),
            presenting: state.unsubscribe
        ) { _ in
            Button("Unsubscribe", role: .destructive) { Task { await model.confirmUnsubscribe() } }
            Button("Cancel", role: .cancel) { model.cancelUnsubscribe() }
        } message: { confirm in
            Text(Self.unsubscribeDetail(confirm.target))
        }
        .environment(\.openURL, OpenURLAction { url in
            model.openLink(url)
            return .handled
        })
        .id(state.thread.id)
    }

    private var counts: String {
        let messages = state.messages.count
        let people = state.thread.participants.count
        return "\(messages) \(messages == 1 ? "message" : "messages") · \(people) \(people == 1 ? "participant" : "participants")"
    }

    private var unsubscribeTitle: String {
        "Unsubscribe from \(state.unsubscribe.map { Sender.name($0.sender) } ?? "this sender")?"
    }

    static func unsubscribeDetail(_ target: UnsubscribeTarget) -> String {
        switch target {
        case .https(let url, true): "Mach sends the sender's one-click unsubscribe request to \(URL(string: url)?.host ?? url)."
        case .https(let url, false): "Opens the sender's unsubscribe page (\(URL(string: url)?.host ?? url)) in your browser."
        case .mailto(let to, _): "Mach sends an unsubscribe email from your account to \(to.split(separator: "@").last.map { "an address at \($0)" } ?? to)."
        }
    }
}

private struct MessageCard: View {
    let model: AppModel
    let state: ReaderState
    let message: Message
    let selected: Bool
    let expanded: Bool
    let tap: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
                .contentShape(Rectangle())
                .onTapGesture(perform: tap)
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
                if let invite = message.calendar {
                    InviteCard(model: model, message: message, invite: invite, reply: state.inviteReplies[message.id])
                }
                messageBody
                if !message.attachments.isEmpty {
                    AttachmentsView(model: model, message: message)
                }
            }
        }
        .padding(14)
        .background(selected ? AnyShapeStyle(.background) : AnyShapeStyle(.quaternary.opacity(0.35)), in: RoundedRectangle(cornerRadius: 10))
        .overlay {
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(selected ? Color.accentColor.opacity(0.55) : Color.primary.opacity(0.08), lineWidth: selected ? 1.5 : 1)
        }
        .shadow(color: .black.opacity(selected ? 0.12 : 0), radius: 8, y: 3)
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
                    if selected, message.headers?.listUnsubscribe != nil {
                        Button("Unsubscribe") {
                            Task {
                                do { try await model.beginUnsubscribe(messageId: message.id) } catch { model.show(error) }
                            }
                        }
                        .buttonStyle(.link)
                        .font(.callout)
                        .help("Unsubscribe from this sender (⌃U)")
                    }
                }
                .lineLimit(1)
                if expanded {
                    Text("to \(message.to.isEmpty ? "—" : message.to.joined(separator: ", "))")
                        .font(.callout).foregroundStyle(.secondary).lineLimit(2)
                } else {
                    Text(message.bodyPlain.map { String($0.prefix(200)) } ?? message.snippet.htmlUnescaped)
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
            let blockRemote = model.blocksRemoteImages(for: message)
            let blocked = blockRemote ? EmailHTML.remoteImageCount(html) : 0
            if blocked > 0 {
                RemoteImagesBar(model: model, message: message, count: blocked)
            }
            EmailWebView(html: EmailHTML.document(for: message), blockRemote: blockRemote, engine: model.engine)
        } else {
            Text(EmailHTML.linkified(message.bodyPlain ?? message.snippet.htmlUnescaped))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

private struct RemoteImagesBar: View {
    let model: AppModel
    let message: Message
    let count: Int

    var body: some View {
        let email = Sender.email(message.from).lowercased()
        HStack(spacing: 6) {
            Group {
                Image(systemName: "photo.badge.exclamationmark")
                Text("\(count) remote \(count == 1 ? "image" : "images") blocked")
            }
            .foregroundStyle(.secondary)
            Button("Show Images") { model.showRemoteImages(messageId: message.id) }
            if !email.isEmpty {
                Text("·").foregroundStyle(.tertiary)
                Button("Always Show from \(email)") { model.alwaysShowRemoteImages(from: email) }
            }
        }
        .buttonStyle(.link)
        .font(.callout)
        .lineLimit(1)
    }
}

private struct AttachmentsView: View {
    let model: AppModel
    let message: Message

    var body: some View {
        let images = message.attachments.filter(\.isImage)
        let files = message.attachments.filter { !$0.isImage }
        VStack(alignment: .leading, spacing: 8) {
            if !images.isEmpty {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 120, maximum: 160), spacing: 8)], alignment: .leading, spacing: 8) {
                    ForEach(images) { attachment in
                        Button { model.openViewer(message: message, attachmentId: attachment.attachmentId) } label: {
                            AttachmentThumbnail(engine: model.engine, message: message, attachment: attachment)
                        }
                        .buttonStyle(.plain)
                        .help("View \(attachment.filename)")
                    }
                }
            }
            if !files.isEmpty {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 200, maximum: 320), spacing: 8)], alignment: .leading, spacing: 8) {
                    ForEach(files) { attachment in
                        Button { model.saveAttachment(attachment, of: message) } label: {
                            HStack(spacing: 8) {
                                Image(nsImage: NSWorkspace.shared.icon(for: UTType(mimeType: attachment.mimeType) ?? .data))
                                    .resizable().frame(width: 24, height: 24)
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(attachment.filename).lineLimit(1).truncationMode(.middle)
                                    Text(ByteCountFormatter.string(fromByteCount: attachment.size, countStyle: .file))
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer(minLength: 0)
                                Image(systemName: "arrow.down.circle").foregroundStyle(.secondary)
                            }
                            .padding(.horizontal, 10).padding(.vertical, 6)
                            .background(.quaternary.opacity(0.6), in: RoundedRectangle(cornerRadius: 8))
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .help("Save \(attachment.filename) to Downloads")
                    }
                }
            }
        }
    }
}

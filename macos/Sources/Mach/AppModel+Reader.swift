import AppKit

struct ImageViewerState: Equatable {
    var message: Message
    var images: [AttachmentRef]
    var index: Int

    var current: AttachmentRef { images[index] }
}

struct UnsubscribeConfirm: Equatable {
    var sender: String
    var accountId: String
    var target: UnsubscribeTarget
}

enum InviteReply: String {
    case sending, sent, failed
}

/// Reader behavior ported from Thread.svelte and App.svelte: the image viewer,
/// attachments, remote images, unsubscribe, invites, and Copy as Markdown.
extension AppModel {
    /// nil lets the keymap resolve the chord. The image viewer owns the plain
    /// keys while it is open, so j or e cannot act on the thread behind it;
    /// command chords (⌘Q, ⌘W, ⌘K) pass through. The unsubscribe alert gets its own keys.
    func handleReaderKey(_ chord: String) -> Bool? {
        guard let reader else { return nil }
        if reader.unsubscribe != nil { return false }
        if reader.viewer != nil {
            switch chord {
            case "esc": closeViewer()
            case "right", "l": stepViewer(by: 1)
            case "left", "h": stepViewer(by: -1)
            case "ctrl+s": saveViewedImage()
            case let chord where chord.hasPrefix("ctrl+"): return nil
            default: break
            }
            return true
        }
        if chord == "ctrl+shift+c" {
            copyThreadAsMarkdown()
            return true
        }
        return nil
    }

    private func updateReader(_ threadId: String, _ change: (inout ReaderState) -> Void) {
        guard var state = reader, state.thread.id == threadId else { return }
        change(&state)
        reader = state
    }

    func selectMessage(_ index: Int) {
        reader?.selectedMessage = index
    }

    // MARK: Attachments

    func openViewer(message: Message, attachmentId: String) {
        let images = message.attachments.filter(\.isImage)
        guard !images.isEmpty else { return }
        reader?.viewer = ImageViewerState(message: message, images: images, index: images.firstIndex { $0.attachmentId == attachmentId } ?? 0)
    }

    func closeViewer() {
        reader?.viewer = nil
    }

    func stepViewer(by delta: Int) {
        guard var viewer = reader?.viewer else { return }
        viewer.index = (viewer.index + delta + viewer.images.count) % viewer.images.count
        reader?.viewer = viewer
    }

    func saveViewedImage() {
        if let viewer = reader?.viewer { saveAttachment(viewer.current, of: viewer.message) }
    }

    func saveAttachment(_ attachment: AttachmentRef, of message: Message) {
        Task {
            do {
                let path = try await engine.saveAttachment(account: message.accountId, messageId: message.id, attachment: attachment)
                show("Saved to \(path)")
                notice?.path = path
            } catch {
                show(error)
            }
        }
    }

    /// Clicking a notice reveals its saved file, or just dismisses it.
    func activateNotice() {
        if let path = notice?.path { NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: path)]) }
        show(nil)
    }

    // MARK: Remote images

    func blocksRemoteImages(for message: Message) -> Bool {
        guard settings?.blockRemoteImages == true, reader?.shownRemoteImages.contains(message.id) != true else { return false }
        return !remoteImageAllow.contains(Sender.email(message.from).lowercased())
    }

    func showRemoteImages(messageId: String) {
        reader?.shownRemoteImages.insert(messageId)
    }

    func alwaysShowRemoteImages(from email: String) {
        remoteImageAllow.insert(email.lowercased())
    }

    // MARK: Links

    /// Every link in a message body lands here. Other schemes (file:, custom
    /// app schemes) are dropped rather than handed to the system.
    func openLink(_ url: URL) {
        switch url.scheme?.lowercased() {
        case "mailto": Task { await composeMailto(url) }
        case "http", "https": NSWorkspace.shared.open(url)
        default: break
        }
    }

    private func composeMailto(_ url: URL) async {
        guard let parts = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return }
        func field(_ name: String) -> String? {
            parts.queryItems?.first { $0.name.lowercased() == name }?.value
        }
        do {
            try await runCompose(KeyAction(["kind": "compose_new"]))
            guard let composer else { return }
            composer.to = [parts.path, field("to")].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: ", ")
            if let cc = field("cc") { composer.cc = cc }
            if let bcc = field("bcc") { composer.bcc = bcc }
            if let subject = field("subject") { composer.subject = subject }
            if let body = field("body") { composer.body = body + composer.body }
        } catch {
            show(error)
        }
    }

    // MARK: Unsubscribe

    func beginUnsubscribe(messageId: String) async throws {
        guard let state = reader, let message = state.messages.first(where: { $0.id == messageId }) else {
            throw EngineFailure(message: "unsubscribe message not found")
        }
        // The engine orders targets one-click POST, then mailto, then a plain page.
        guard let target = try await engine.unsubscribeTargets(messageId: messageId).first else {
            show("No unsubscribe method advertised")
            return
        }
        updateReader(state.thread.id) { $0.unsubscribe = UnsubscribeConfirm(sender: message.from, accountId: message.accountId, target: target) }
    }

    func cancelUnsubscribe() {
        reader?.unsubscribe = nil
    }

    func confirmUnsubscribe() async {
        guard let confirm = reader?.unsubscribe else { return }
        reader?.unsubscribe = nil
        do {
            switch confirm.target {
            case .https(let url, true):
                try await engine.unsubscribePost(url: url)
                show("Unsubscribed")
            case .https(let url, false):
                if let url = URL(string: url) { NSWorkspace.shared.open(url) }
                show("Opened the unsubscribe page")
            case .mailto(let to, let subject):
                try await engine.unsubscribeMailto(account: confirm.accountId, to: to, subject: subject)
                show("Unsubscribed")
            }
        } catch {
            show(error)
        }
    }

    // MARK: Invites

    func respondToInvite(messageId: String, response: String) async {
        guard let threadId = reader?.thread.id else { return }
        updateReader(threadId) { $0.inviteReplies[messageId] = .sending }
        do {
            _ = try await engine.dispatch(KeyAction(["kind": "respond_to_invite", "message_id": messageId, "response": response]))
            let report = try await engine.flushOutbox()
            if report.failed > 0 { throw EngineFailure(message: report.lastError ?? "calendar reply failed") }
            updateReader(threadId) { $0.inviteReplies[messageId] = .sent }
        } catch {
            updateReader(threadId) { $0.inviteReplies[messageId] = .failed }
            show(error)
        }
        await refreshOutbox()
    }

    // MARK: Markdown

    func copyThreadAsMarkdown() {
        guard let reader else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(ThreadMarkdown.render(reader.thread, reader.messages), forType: .string)
        show("Copied thread as Markdown")
    }
}

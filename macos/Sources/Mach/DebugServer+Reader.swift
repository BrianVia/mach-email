import Foundation

/// Debug-server hooks for the reader. Message and attachment indexes default
/// to the selected message and its first matching attachment.
extension AppModel {
    static let debugReaderActions = [
        "select_message", "open_image", "save_attachment", "activate_notice", "copy_markdown",
        "block_remote_images", "show_images", "unsubscribe", "unsubscribe_cancel",
        // Replaces the confirm's target so the confirm path can run against a URL
        // that never reaches a real sender.
        "unsubscribe_inject", "unsubscribe_confirm",
    ]

    /// Returns an error message, or nil on success.
    func debugReaderAction(_ name: String, _ object: [String: Any]) async -> String? {
        if name == "block_remote_images" {
            // In memory only; settings.toml is the owner's.
            settings?.blockRemoteImages = object["value"] as? Bool ?? true
            return nil
        }
        if name == "activate_notice" {
            activateNotice()
            return nil
        }
        guard let state = reader else { return "no thread open" }
        let index = object["message"] as? Int ?? state.selectedMessage
        guard state.messages.indices.contains(index) else { return "no message \(index)" }
        let message = state.messages[index]
        let attachmentIndex = object["index"] as? Int ?? 0
        switch name {
        case "select_message": selectMessage(index)
        case "open_image":
            let images = message.attachments.filter(\.isImage)
            guard images.indices.contains(attachmentIndex) else { return "message \(index) has \(images.count) images" }
            openViewer(message: message, attachmentId: images[attachmentIndex].attachmentId)
        case "save_attachment":
            let files = message.attachments.filter { !$0.isImage }
            guard files.indices.contains(attachmentIndex) else { return "message \(index) has \(files.count) non-image attachments" }
            saveAttachment(files[attachmentIndex], of: message)
            try? await Task.sleep(for: .milliseconds(500))
        case "copy_markdown": copyThreadAsMarkdown()
        case "show_images": showRemoteImages(messageId: message.id)
        case "unsubscribe":
            do { try await beginUnsubscribe(messageId: message.id) } catch { return String(describing: error) }
        case "unsubscribe_cancel": cancelUnsubscribe()
        case "unsubscribe_inject":
            guard let url = object["url"] as? String, url.hasPrefix("http://127.0.0.1:") else {
                return "unsubscribe_inject takes a loopback url, e.g. http://127.0.0.1:7373/noop"
            }
            reader?.unsubscribe = UnsubscribeConfirm(sender: message.from, accountId: message.accountId, target: .https(url: url, oneClick: true))
        case "unsubscribe_confirm":
            guard case .https(let url, true) = state.unsubscribe?.target, url.hasPrefix("http://127.0.0.1:") else {
                return "unsubscribe_confirm only runs an injected loopback target"
            }
            await confirmUnsubscribe()
        default: return "unknown reader action \(name)"
        }
        return nil
    }

    var debugReaderState: [String: Any] {
        guard let state = reader else { return [:] }
        let message = state.currentMessage
        let confirm: Any = state.unsubscribe.map { confirm -> [String: Any] in
            switch confirm.target {
            case .https(let url, let oneClick): ["sender": confirm.sender, "kind": "https", "url": url, "one_click": oneClick]
            case .mailto(let to, let subject): ["sender": confirm.sender, "kind": "mailto", "to": to, "subject": subject]
            }
        } ?? NSNull()
        return [
            "selected_message_id": message?.id ?? NSNull(),
            "selected_from": message?.from ?? NSNull(),
            "initials": message.map { Sender.initials($0.from) } ?? NSNull(),
            "attachments": message?.attachments.map { ["filename": $0.filename, "mime": $0.mimeType, "image": $0.isImage] } ?? [],
            "has_unsubscribe": message?.headers?.listUnsubscribe != nil,
            "calendar": message?.calendar?.summary ?? NSNull(),
            "viewer": state.viewer.map { ["index": $0.index + 1, "count": $0.images.count, "filename": $0.current.filename] } ?? NSNull(),
            "unsubscribe_confirm": confirm,
            "block_remote_images": settings?.blockRemoteImages ?? false,
            "remote_blocked": state.messages.filter(blocksRemoteImages).map { ["id": $0.id, "images": EmailHTML.remoteImageCount($0.bodyHtml ?? "")] },
            "remote_image_allow": remoteImageAllow.sorted(),
            "invite_replies": state.inviteReplies.mapValues(\.rawValue),
            "notice_path": notice?.path ?? NSNull(),
        ]
    }
}

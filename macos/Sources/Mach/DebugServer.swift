import AppKit
import Network
import UserNotifications
import WebKit

/// Loopback HTTP server that lets an agent see and drive the app over SSH.
/// Everything runs on the main queue so handlers can touch AppKit directly.
@MainActor
final class DebugServer {
    private struct Request {
        let method: String
        let path: String
        let body: Data

        /// Returns nil until `buffer` holds the full headers and body.
        init?(parsing buffer: Data) {
            guard let headerEnd = buffer.range(of: Data("\r\n\r\n".utf8)),
                let head = String(data: buffer[..<headerEnd.lowerBound], encoding: .utf8)
            else { return nil }
            let lines = head.components(separatedBy: "\r\n")
            let requestLine = lines[0].split(separator: " ")
            guard requestLine.count >= 2 else { return nil }
            let length = lines.dropFirst()
                .first { $0.lowercased().hasPrefix("content-length:") }
                .flatMap { Int($0.dropFirst("content-length:".count).trimmingCharacters(in: .whitespaces)) } ?? 0
            guard buffer.count - headerEnd.upperBound >= length else { return nil }
            method = String(requestLine[0])
            path = String(requestLine[1])
            body = buffer[headerEnd.upperBound..<headerEnd.upperBound + length]
        }
    }

    private struct Response {
        var status = 200
        var contentType = "application/json"
        var body: Data

        static func json(_ object: Any, status: Int = 200) -> Response {
            let data = (try? JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted, .sortedKeys])) ?? Data()
            return Response(status: status, body: data + Data("\n".utf8))
        }

        static func error(_ status: Int, _ message: String) -> Response { .json(["error": message], status: status) }
    }

    private struct KeyRequest: Decodable {
        let key: String
        var cmd, shift, alt, ctrl: Bool?
    }

    private struct NotifyRequest: Decodable {
        let title: String
        var body: String?
        let thread_id: String
    }

    let model: AppModel
    private let listener: NWListener

    init?(port: UInt16, model: AppModel) {
        self.model = model
        let params = NWParameters.tcp
        params.allowLocalEndpointReuse = true
        params.requiredLocalEndpoint = .hostPort(host: .ipv4(.loopback), port: NWEndpoint.Port(rawValue: port)!)
        guard let listener = try? NWListener(using: params) else { return nil }
        self.listener = listener
        listener.newConnectionHandler = { [weak self] connection in
            MainActor.assumeIsolated {
                connection.start(queue: .main)
                self?.receive(on: connection, buffer: Data())
            }
        }
        listener.stateUpdateHandler = { state in
            if case .failed(let error) = state { NSLog("mach debug server on port \(port) failed: \(error)") }
        }
        listener.start(queue: .main)
    }

    private func receive(on connection: NWConnection, buffer: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 1 << 16) { [weak self] data, _, isComplete, error in
            MainActor.assumeIsolated {
                guard let self else { return connection.cancel() }
                let buffer = buffer + (data ?? Data())
                if let request = Request(parsing: buffer) {
                    Task { self.send(await self.route(request), on: connection) }
                } else if isComplete || error != nil {
                    connection.cancel()
                } else {
                    self.receive(on: connection, buffer: buffer)
                }
            }
        }
    }

    private func send(_ response: Response, on connection: NWConnection) {
        let head = "HTTP/1.1 \(response.status) \(HTTPURLResponse.localizedString(forStatusCode: response.status))\r\n"
            + "Content-Type: \(response.contentType)\r\nContent-Length: \(response.body.count)\r\nConnection: close\r\n\r\n"
        connection.send(content: Data(head.utf8) + response.body, completion: .contentProcessed { _ in connection.cancel() })
    }

    private func route(_ request: Request) async -> Response {
        switch (request.method, request.path) {
        case ("GET", "/health"): return .json(["ok": true])
        case ("GET", "/state"): return .json(await state())
        case ("GET", "/snapshot"): return await snapshot()
        case ("POST", "/key"): return await key(request.body)
        case ("POST", "/action"): return await action(request.body)
        case ("POST", "/notify"): return await notify(request.body)
        default: return .error(404, "no route \(request.method) \(request.path)")
        }
    }

    private var window: NSWindow? {
        NSApp.keyWindow ?? NSApp.mainWindow ?? NSApp.windows.first { $0.isVisible && $0.canBecomeMain }
    }

    private func state() async -> [String: Any] {
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        let authorization = switch settings.authorizationStatus {
        case .notDetermined: "not_determined"
        case .denied: "denied"
        case .authorized: "authorized"
        case .provisional: "provisional"
        @unknown default: "unknown(\(settings.authorizationStatus.rawValue))"
        }
        func setting(_ value: UNNotificationSetting) -> String {
            switch value {
            case .enabled: "enabled"
            case .disabled: "disabled"
            case .notSupported: "not_supported"
            @unknown default: "unknown"
            }
        }
        let visible = model.visibleThreads
        let reader = model.reader
        return [
            "route": model.route.name,
            "label": model.list.label,
            "account_filter": model.list.account ?? NSNull(),
            "split": model.split.rawValue,
            "selected_thread_id": model.list.selection ?? NSNull(),
            "selected_index": visible.firstIndex { $0.id == model.list.selection } ?? NSNull(),
            "opened_thread_id": reader?.thread.id ?? NSNull(),
            "opened_thread": reader.map { reader in
                [
                    "subject": reader.thread.subject,
                    "starred": reader.thread.starred,
                    "unread": reader.thread.unread,
                    "message_count": reader.messages.count,
                    "selected_message": reader.selectedMessage,
                    "html_messages": reader.messages.filter { !($0.bodyHtml ?? "").isEmpty }.count,
                    "fetched_full": reader.messages.allSatisfy(\.fetchedFull),
                ] as [String: Any]
            } ?? NSNull(),
            "thread_count": visible.count,
            "loaded_thread_count": model.list.threads.count,
            "visible_threads": visible.prefix(12).map {
                ["id": $0.id, "subject": $0.subject, "unread": $0.unread, "starred": $0.starred, "account": $0.accountId]
            },
            "labels": model.labels.count,
            "accounts": model.accounts,
            "online": model.online,
            "sync_ok": model.syncOkByAccount,
            "outbox": ["pending": model.outbox.pending, "failed": model.outbox.failed],
            "last_mail_synced_at": model.lastMailSyncedAt.map { $0.formatted(.iso8601) } ?? NSNull(),
            "notice": model.notice?.text ?? NSNull(),
            "chord_buffer": model.chordBuffer,
            "system": systemState(),
            "window_title": window?.title ?? NSNull(),
            "appearance": NSApp.effectiveAppearance.name.rawValue,
            "app_active": NSApp.isActive,
            "notifications": [
                "authorization": authorization,
                "request": model.notificationsAllowed.map { $0 ? "allowed" : "refused" } ?? "awaiting_user",
                "alert": setting(settings.alertSetting),
                "sound": setting(settings.soundSetting),
                "notification_center": setting(settings.notificationCenterSetting),
            ],
        ]
    }

    private func key(_ body: Data) async -> Response {
        guard let request = try? JSONDecoder().decode(KeyRequest.self, from: body) else {
            return .error(400, #"expected {"key": "j", "cmd"?, "shift"?, "alt"?, "ctrl"?}"#)
        }
        let held = Set([("cmd", request.cmd), ("shift", request.shift), ("alt", request.alt), ("ctrl", request.ctrl)]
            .filter { $0.1 == true }.map(\.0))
        guard let event = KeyChord.event(key: request.key, modifiers: held, window: window) else {
            return .error(400, "could not synthesize key \(request.key)")
        }
        NSApp.postEvent(event, atStart: false)
        // Let the run loop dispatch the posted event so the reply shows its effect.
        try? await Task.sleep(for: .milliseconds(50))
        return .json(await state())
    }

    /// Keymap action names (with their usual fields filled from the current
    /// selection) plus a few debug-only names.
    static let actionNames = [
        "select_next", "select_prev", "open_thread", "back_to_list", "open_label", "load_older", "refresh",
        "inbox_split_important", "inbox_split_other", "inbox_split_updates", "inbox_split_newsletters",
        "star", "unstar", "mark_read", "mark_unread", "archive", "trash", "mute", "show_activity",
        "retry_outbox", "account", "appearance",
    ] + systemActionNames

    private func action(_ body: Data) async -> Response {
        guard var object = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any],
            let name = object.removeValue(forKey: "name") as? String, Self.actionNames.contains(name)
        else {
            return .error(400, "unknown action; known: \(Self.actionNames)")
        }
        let context = model.keyContext
        switch name {
        case let name where Self.systemActionNames.contains(name):
            return .json(await systemAction(name, object))
        case "appearance":
            let value = object["value"] as? String
            NSApp.appearance = value == "dark" ? NSAppearance(named: .darkAqua) : value == "light" ? NSAppearance(named: .aqua) : nil
            try? await Task.sleep(for: .milliseconds(300))
            return .json(["ok": true, "appearance": NSApp.effectiveAppearance.name.rawValue])
        case "account":
            model.selectAccount(object["account"] as? String)
            return .json(await state())
        case "retry_outbox":
            await model.retryFailedChanges()
            return .json(await state())
        case "unstar": object["starred"] = false
        case "star": object["starred"] = object["starred"] ?? true
        case "mark_unread": object["read"] = false
        case "mark_read": object["read"] = object["read"] ?? true
        case "open_thread": object["id"] = object["id"] ?? context.currentThread
        default: break
        }
        object["kind"] = ["unstar": "star", "mark_unread": "mark_read"][name] ?? name
        if ["star", "mark_read", "archive", "trash", "mute"].contains(object["kind"] as? String), object["thread_ids"] == nil {
            object["thread_ids"] = context.selection
        }
        do {
            await model.run(try KeyAction(object))
        } catch {
            return .error(400, "bad action: \(error)")
        }
        return .json(await state())
    }

    private func notify(_ body: Data) async -> Response {
        guard let request = try? JSONDecoder().decode(NotifyRequest.self, from: body) else {
            return .error(400, #"expected {"title", "body"?, "thread_id"}"#)
        }
        let content = UNMutableNotificationContent()
        content.title = request.title
        content.body = request.body ?? ""
        content.threadIdentifier = request.thread_id
        content.userInfo = ["thread_id": request.thread_id]
        let id = UUID().uuidString
        do {
            try await UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: id, content: content, trigger: nil))
            return .json(["ok": true, "id": id])
        } catch {
            return .error(500, String(describing: error))
        }
    }

    /// WKWebView draws out of process, so `cacheDisplay` leaves it blank;
    /// each visible web view's own snapshot is drawn over its frame.
    private func snapshot() async -> Response {
        // The frame view (contentView's superview) includes the title bar and toolbar.
        guard let view = window?.contentView?.superview ?? window?.contentView, let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else {
            return .error(404, "no window")
        }
        view.cacheDisplay(in: view.bounds, to: rep)
        var overlays: [(NSImage, NSRect, NSRect)] = []
        for webView in Self.webViews(in: view) where !webView.visibleRect.isEmpty {
            guard let image = try? await webView.takeSnapshot(configuration: nil) else { continue }
            overlays.append((image, webView.convert(webView.bounds, to: view), webView.convert(webView.visibleRect, to: view)))
        }
        if !overlays.isEmpty, let context = NSGraphicsContext(bitmapImageRep: rep) {
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = context
            for (image, frame, visible) in overlays {
                NSGraphicsContext.saveGraphicsState()
                visible.clip()
                image.draw(in: frame)
                NSGraphicsContext.restoreGraphicsState()
            }
            NSGraphicsContext.restoreGraphicsState()
        }
        guard let png = rep.representation(using: .png, properties: [:]) else { return .error(500, "png encoding failed") }
        return Response(contentType: "image/png", body: png)
    }

    private static func webViews(in view: NSView) -> [WKWebView] {
        if let webView = view as? WKWebView { return [webView] }
        return view.subviews.flatMap(webViews)
    }
}

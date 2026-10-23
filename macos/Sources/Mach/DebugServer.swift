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
        /// Lowercased names.
        let headers: [String: String]
        let body: Data

        enum Parse {
            case incomplete
            case malformed(Int, String)
            case request(Request)
        }

        static func parse(_ buffer: Data) -> Parse {
            guard let headerEnd = buffer.range(of: Data("\r\n\r\n".utf8)) else {
                return buffer.count > maxRequestBytes ? .malformed(413, "request too large") : .incomplete
            }
            guard let head = String(data: buffer[..<headerEnd.lowerBound], encoding: .utf8) else { return .malformed(400, "headers are not UTF-8") }
            let lines = head.components(separatedBy: "\r\n")
            let requestLine = lines[0].split(separator: " ")
            guard requestLine.count >= 2 else { return .malformed(400, "bad request line") }
            var headers: [String: String] = [:]
            for line in lines.dropFirst() {
                guard let colon = line.firstIndex(of: ":") else { return .malformed(400, "bad header line") }
                headers[line[..<colon].lowercased()] = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            }
            var length = 0
            if let value = headers["content-length"] {
                guard let parsed = Int(value), parsed >= 0 else { return .malformed(400, "bad Content-Length") }
                length = parsed
            }
            guard length <= maxRequestBytes else { return .malformed(413, "request too large") }
            // Every index comes from the buffer itself, and a body still in flight waits for the next read.
            let start = headerEnd.upperBound
            guard buffer.endIndex - start >= length else { return .incomplete }
            return .request(Request(
                method: String(requestLine[0]), path: String(requestLine[1]), headers: headers, body: Data(buffer[start..<start + length])))
        }
    }

    private static let maxRequestBytes = 1 << 20

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
    private let port: UInt16

    init?(port: UInt16, model: AppModel) {
        self.model = model
        self.port = port
        guard let endpointPort = NWEndpoint.Port(rawValue: port) else { return nil }
        let params = NWParameters.tcp
        params.allowLocalEndpointReuse = true
        params.requiredLocalEndpoint = .hostPort(host: .ipv4(.loopback), port: endpointPort)
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
                switch Request.parse(buffer) {
                case .request(let request):
                    Task { self.send(await self.route(request), on: connection) }
                case .malformed(let status, let message):
                    self.send(.error(status, message), on: connection)
                case .incomplete where isComplete || error != nil:
                    connection.cancel()
                case .incomplete:
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

    /// Web pages can reach loopback too. A page's fetch carries Origin or
    /// Sec-Fetch-Site, a DNS-rebound page carries its own Host, and a custom
    /// header on a cross-origin POST forces a CORS preflight this server never answers.
    private func rejection(_ request: Request) -> Response? {
        guard ["127.0.0.1:\(port)", "localhost:\(port)"].contains(request.headers["host"] ?? "") else {
            return .error(403, "Host must be 127.0.0.1:\(port) or localhost:\(port)")
        }
        if request.headers["origin"] != nil || request.headers["sec-fetch-site"] != nil {
            return .error(403, "browser requests are refused")
        }
        if request.method != "GET", request.headers["x-mach-debug"] != "1" {
            return .error(403, "POST requires the header X-Mach-Debug: 1")
        }
        return nil
    }

    private func route(_ request: Request) async -> Response {
        if let rejection = rejection(request) { return rejection }
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

    /// The composer sheet when one is attached, so keys and snapshots reach it.
    private var window: NSWindow? {
        let base = NSApp.keyWindow ?? NSApp.mainWindow ?? NSApp.windows.first { $0.isVisible && $0.canBecomeMain }
        return base?.attachedSheet ?? base
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
                    "reader": model.debugReaderState,
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
            "composer": model.composerDebugState,
            "scheduled": model.scheduledDebugState,
            "chord_buffer": model.chordBuffer,
            "system": systemState(),
            "palette": model.palette.map { palette in
                let commands = palette.filtered
                return [
                    "query": palette.query,
                    "count": commands.count,
                    "selected": palette.selectedCommand?.title ?? NSNull(),
                    "commands": commands.prefix(40).map { ["title": $0.title, "chord": $0.chord ?? ""] },
                ] as [String: Any]
            } ?? NSNull(),
            "search": model.search.map { search in
                [
                    "query": search.query,
                    "answered": search.answered,
                    "count": search.results.count,
                    "selected": search.selected,
                    "results": search.results.prefix(8).map { ["id": $0.id, "subject": $0.subject] },
                ] as [String: Any]
            } ?? NSNull(),
            "activity": { () -> Any in
                guard case .activity(let activity) = model.route else { return NSNull() }
                return [
                    "count": activity.entries.count,
                    "selected": activity.selected,
                    "entries": activity.entries.prefix(10).map {
                        ["id": $0.id, "kind": $0.kind, "summary": $0.summary, "state": $0.state, "undone": $0.undone, "undoable": $0.undoable]
                    },
                ] as [String: Any]
            }(),
            "menu": (NSApp.mainMenu?.items ?? []).map { top in
                [
                    "title": top.title,
                    "items": (top.submenu?.items ?? []).filter { !$0.isSeparatorItem }.map { item in
                        let modifiers: [(NSEvent.ModifierFlags, String)] = [(.control, "⌃"), (.option, "⌥"), (.shift, "⇧"), (.command, "⌘")]
                        let key = item.keyEquivalent.isEmpty
                            ? "" : modifiers.filter { item.keyEquivalentModifierMask.contains($0.0) }.map(\.1).joined() + item.keyEquivalent.uppercased()
                        return ["title": item.title, "key": key, "raw": item.keyEquivalent, "mask": item.keyEquivalentModifierMask.rawValue, "enabled": item.isEnabled]
                    },
                ] as [String: Any]
            },
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
        "retry_outbox", "account", "appearance", "search", "palette", "undo_activity", "menu",
        "compose_new", "reply", "reply_all", "forward", "save_draft", "compose_fields", "attach", "close_composer", "discard_draft",
    ] + AppModel.debugReaderActions + systemActionNames

    /// Whole menus or single items the `menu` action may perform. The Message
    /// menu (archive, trash, mute), Retry Failed Changes (resends mail), Edit's
    /// mail undo, and the app menu (quit) are left out.
    static let menuAllowList: Set<String> = ["View", "Mailbox > Sync Now", "Mailbox > Load Older Mail", "File > New Message"]

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
        case let name where AppModel.debugReaderActions.contains(name):
            if let error = await model.debugReaderAction(name, object) { return .error(400, error) }
            return .json(await state())
        case "compose_fields", "attach", "close_composer", "discard_draft":
            do { try await model.debugCompose(name, object) } catch { return .error(400, String(describing: error)) }
            try? await Task.sleep(for: .milliseconds(300))
            return .json(await state())
        case "reply", "reply_all", "forward":
            object["message_id"] = object["message_id"] ?? context.currentMessage
            if name != "forward" { object["all"] = name == "reply_all" }
        case "save_draft": object["draft_id"] = context.currentDraft
        case "palette":
            model.togglePalette()
            return .json(await state())
        case "menu":
            // Performs the real NSMenuItem, e.g. {"path": ["Mailbox", "Sync Now"]}.
            guard let path = object["path"] as? [String], path.count == 2,
                Self.menuAllowList.contains(path[0]) || Self.menuAllowList.contains(path.joined(separator: " > "))
            else {
                return .error(403, "menu takes only navigation and sync items: \(Self.menuAllowList.sorted())")
            }
            guard
                let menu = NSApp.mainMenu?.items.first(where: { $0.title == path[0] })?.submenu,
                let index = menu.items.firstIndex(where: { $0.title == path[1] })
            else {
                return .error(400, #"expected {"path": ["Menu", "Item"]} naming an existing menu item"#)
            }
            menu.update()
            guard menu.items[index].isEnabled else { return .error(409, "menu item \(path) is disabled") }
            menu.performActionForItem(at: index)
            try? await Task.sleep(for: .milliseconds(100))
            return .json(await state())
        case "unstar": object["starred"] = false
        case "star": object["starred"] = object["starred"] ?? true
        case "mark_unread": object["read"] = false
        case "mark_read": object["read"] = object["read"] ?? true
        case "open_thread": object["id"] = object["id"] ?? context.currentThread
        default: break
        }
        object["kind"] = ["unstar": "star", "mark_unread": "mark_read", "reply_all": "reply"][name] ?? name
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
        let window = window.map { $0.sheetParent ?? $0 }
        // The frame view (contentView's superview) includes the title bar and toolbar.
        guard let view = window?.contentView?.superview ?? window?.contentView, let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else {
            return .error(404, "no window")
        }
        view.cacheDisplay(in: view.bounds, to: rep)
        var overlays: [(NSImage, NSRect, NSRect)] = []
        // A web view under a SwiftUI overlay (the image viewer) loses the hit test and stays hidden.
        for webView in Self.webViews(in: view) where !webView.visibleRect.isEmpty
            && view.hitTest(webView.convert(NSPoint(x: webView.visibleRect.midX, y: webView.visibleRect.midY), to: view))?.isDescendant(of: webView) == true
        {
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
        // Alerts are sheets, separate windows the frame view does not draw.
        if let window, let sheet = window.attachedSheet, let sheetView = sheet.contentView?.superview,
            let sheetRep = sheetView.bitmapImageRepForCachingDisplay(in: sheetView.bounds),
            let context = NSGraphicsContext(bitmapImageRep: rep)
        {
            sheetView.cacheDisplay(in: sheetView.bounds, to: sheetRep)
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = context
            sheetRep.draw(in: sheet.frame.offsetBy(dx: -window.frame.minX, dy: -window.frame.minY))
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

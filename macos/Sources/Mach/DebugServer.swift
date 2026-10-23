import AppKit
import Network
import UserNotifications

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

    private let model: AppModel
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
        case ("GET", "/snapshot"): return snapshot()
        case ("POST", "/key"): return await key(request.body)
        case ("POST", "/action"): return action(request.body)
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
        return [
            "view": model.view,
            "selected_label_id": model.selectedLabelId ?? NSNull(),
            "selected_thread_id": model.selectedThreadId ?? NSNull(),
            "opened_thread_id": model.openedThreadId ?? NSNull(),
            "thread_ids": model.threads.map(\.id),
            "account": model.account?.email ?? NSNull(),
            "last_error": model.lastError ?? NSNull(),
            "window_title": window?.title ?? NSNull(),
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

    private func action(_ body: Data) -> Response {
        let name = (try? JSONSerialization.jsonObject(with: body) as? [String: Any])?["name"] as? String
        guard let name, let action = AppModel.Action(rawValue: name) else {
            return .error(400, "unknown action; known: \(AppModel.Action.allCases.map(\.rawValue))")
        }
        model.perform(action)
        return .json(["ok": true, "action": name])
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

    private func snapshot() -> Response {
        // The frame view (contentView's superview) includes the title bar and toolbar.
        guard let view = window?.contentView?.superview ?? window?.contentView, let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else {
            return .error(404, "no window")
        }
        view.cacheDisplay(in: view.bounds, to: rep)
        guard let png = rep.representation(using: .png, properties: [:]) else { return .error(500, "png encoding failed") }
        return Response(contentType: "image/png", body: png)
    }
}

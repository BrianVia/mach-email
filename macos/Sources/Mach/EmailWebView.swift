import SwiftUI
import WebKit

/// Builds what the reader renders: HTML documents for the web view and
/// linkified plain text.
enum EmailHTML {
    static let attachmentScheme = "mach"

    static func attachmentURL(account: String, messageId: String, attachmentId: String) -> String {
        let parts = [account, messageId, attachmentId].map { $0.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(["/"])) ?? $0 }
        return "\(attachmentScheme)://attachment/\(parts.joined(separator: "/"))"
    }

    /// Wraps the message body in a light, readable page and points `cid:`
    /// images at the attachment scheme handler.
    static func document(for message: Message) -> String {
        var inline: [String: String] = [:]
        for image in message.inlineImages {
            inline[image.contentId.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "<> "))] = image.attachmentId
        }
        let body = (message.bodyHtml ?? "").replacing(/(?i)cid:([^"'\s>)]+)/) { match in
            guard let attachmentId = inline[String(match.1).lowercased()] else { return String(match.0) }
            return attachmentURL(account: message.accountId, messageId: message.id, attachmentId: attachmentId)
        }
        return """
            <!doctype html><html><head><meta charset="utf-8">
            <meta name="color-scheme" content="light">
            <style>
            html, body { height: auto !important; margin: 0; background: #fff; }
            body { padding: 16px; font: 14px/1.45 -apple-system, BlinkMacSystemFont, "Helvetica Neue", sans-serif; color: #1d1d1f; overflow-wrap: anywhere; }
            img { max-width: 100%; }
            pre { white-space: pre-wrap; }
            blockquote { margin-left: 0.5em; padding-left: 0.8em; border-left: 3px solid #d2d2d7; color: #555; }
            </style></head><body>\(body)</body></html>
            """
    }

    static func linkified(_ text: String) -> AttributedString {
        var attributed = AttributedString(text)
        guard let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) else { return attributed }
        for match in detector.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
            guard let url = match.url, let range = Range(match.range, in: text),
                let attributedRange = Range(range, in: attributed)
            else { continue }
            attributed[attributedRange].link = url
        }
        return attributed
    }
}

/// An HTML email body. JavaScript is off, links open in the default browser,
/// every other navigation is refused, and the view grows to fit its content
/// so the reader's ScrollView does the scrolling.
struct EmailWebView: View {
    let html: String
    let engine: Engine
    @State private var height: CGFloat = 40

    var body: some View {
        WebViewRepresentable(html: html, engine: engine, height: $height)
            .frame(height: height)
            .clipShape(RoundedRectangle(cornerRadius: 6))
    }
}

private struct WebViewRepresentable: NSViewRepresentable {
    let html: String
    let engine: Engine
    @Binding var height: CGFloat

    func makeCoordinator() -> Coordinator { Coordinator(height: $height) }

    func makeNSView(context: Context) -> FittingWebView {
        let config = WKWebViewConfiguration()
        config.defaultWebpagePreferences.allowsContentJavaScript = false
        config.setURLSchemeHandler(AttachmentSchemeHandler(engine: engine), forURLScheme: EmailHTML.attachmentScheme)
        let view = FittingWebView(frame: .zero, configuration: config)
        view.navigationDelegate = context.coordinator
        view.onWidthChange = { [weak coordinator = context.coordinator, weak view] in
            if let view { coordinator?.measure(view) }
        }
        context.coordinator.load(html, in: view)
        return view
    }

    func updateNSView(_ view: FittingWebView, context: Context) {
        context.coordinator.height = $height
        context.coordinator.load(html, in: view)
    }

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate {
        var height: Binding<CGFloat>
        private var loaded: String?

        init(height: Binding<CGFloat>) { self.height = height }

        func load(_ html: String, in view: WKWebView) {
            guard html != loaded else { return }
            loaded = html
            view.loadHTMLString(html, baseURL: nil)
        }

        func measure(_ view: WKWebView) {
            // Page scripts are off; this runs in the app's own content world.
            view.evaluateJavaScript("Math.ceil(document.documentElement.getBoundingClientRect().height)") { [height] result, _ in
                if let value = result as? NSNumber, value.doubleValue > 0 { height.wrappedValue = CGFloat(value.doubleValue) }
            }
        }

        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
            guard let url = action.request.url else { return .cancel }
            if action.navigationType == .linkActivated {
                if ["http", "https", "mailto"].contains(url.scheme?.lowercased() ?? "") { NSWorkspace.shared.open(url) }
                return .cancel
            }
            return url.absoluteString == "about:blank" ? .allow : .cancel
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            measure(webView)
        }
    }
}

private final class FittingWebView: WKWebView {
    var onWidthChange: (() -> Void)?

    override func scrollWheel(with event: NSEvent) {
        nextResponder?.scrollWheel(with: event)
    }

    override func setFrameSize(_ newSize: NSSize) {
        let widthChanged = newSize.width != frame.width
        super.setFrameSize(newSize)
        if widthChanged { onWidthChange?() }
    }
}

/// Serves `mach://attachment/<account>/<message>/<attachment>` from the engine,
/// which caches attachment bytes on disk.
@MainActor
private final class AttachmentSchemeHandler: NSObject, WKURLSchemeHandler {
    private let engine: Engine
    private var stopped: Set<ObjectIdentifier> = []

    init(engine: Engine) { self.engine = engine }

    func webView(_ webView: WKWebView, start task: any WKURLSchemeTask) {
        let parts = Array(task.request.url?.pathComponents.dropFirst() ?? [])
        guard let url = task.request.url, url.host == "attachment", parts.count == 3 else {
            task.didFailWithError(URLError(.badURL))
            return
        }
        let id = ObjectIdentifier(task)
        Task {
            let result: Result<(data: Data, mime: String), Error>
            do {
                result = .success(try await engine.attachment(account: parts[0], messageId: parts[1], attachmentId: parts[2]))
            } catch {
                result = .failure(error)
            }
            guard stopped.remove(id) == nil else { return }
            switch result {
            case .success(let file):
                task.didReceive(URLResponse(url: url, mimeType: file.mime, expectedContentLength: file.data.count, textEncodingName: nil))
                task.didReceive(file.data)
                task.didFinish()
            case .failure(let error):
                task.didFailWithError(error)
            }
        }
    }

    func webView(_ webView: WKWebView, stop task: any WKURLSchemeTask) {
        stopped.insert(ObjectIdentifier(task))
    }
}

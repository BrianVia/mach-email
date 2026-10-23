import SwiftUI
import WebKit

/// Builds what the reader renders: HTML documents for the web view and
/// linkified plain text.
enum EmailHTML {
    static let attachmentScheme = "mach"
    static let linkSchemes: Set<String> = ["http", "https", "mailto"]

    static func attachmentURL(account: String, messageId: String, attachmentId: String) -> String {
        let parts = [account, messageId, attachmentId].map { $0.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(["/"])) ?? $0 }
        return "\(attachmentScheme)://attachment/\(parts.joined(separator: "/"))"
    }

    /// Whether a body follows the app's light or dark appearance. Designed
    /// mail paints its own backgrounds and assumes white paper unless it ships
    /// dark-mode rules; simple mail with no colors of its own takes the card's.
    static func followsAppearance(_ html: String) -> Bool {
        if html.contains(/(?i)prefers-color-scheme\s*:\s*dark/) { return true }
        return !html.contains(/(?i)bgcolor\s*=|background[\w-]*\s*:|(^|[^\w-])color\s*[:=]/)
    }

    /// Wraps the message body in a readable page and points `cid:` images at
    /// the attachment scheme handler.
    static func document(for message: Message) -> String {
        var inline: [String: String] = [:]
        for image in message.inlineImages {
            inline[image.contentId.lowercased().trimmingCharacters(in: CharacterSet(charactersIn: "<> "))] = image.attachmentId
        }
        let html = message.bodyHtml ?? ""
        let body = html.replacing(/(?i)cid:([^"'\s>)]+)/) { match in
            guard let attachmentId = inline[String(match.1).lowercased()] else { return String(match.0) }
            return attachmentURL(account: message.accountId, messageId: message.id, attachmentId: attachmentId)
        }
        // The web view draws no background of its own, so an adaptive body shows the card behind it.
        let page = followsAppearance(html)
            ? """
                <meta name="color-scheme" content="light dark">
                <style>
                html, body { background: transparent; }
                @media (prefers-color-scheme: dark) {
                  body { color: rgba(255, 255, 255, 0.86); }
                  a { color: #6cb4ff; }
                  blockquote { border-left-color: #48484a !important; color: #a1a1a6 !important; }
                }
                </style>
                """
            : """
                <meta name="color-scheme" content="light">
                <style>html, body { background: #fff; }</style>
                """
        return """
            <!doctype html><html><head><meta charset="utf-8">
            <style>
            html, body { height: auto !important; margin: 0; }
            body { padding: 16px; font: 14px/1.45 -apple-system, BlinkMacSystemFont, "Helvetica Neue", sans-serif; color: #1d1d1f; overflow-wrap: anywhere; }
            img { max-width: 100%; }
            pre { white-space: pre-wrap; }
            blockquote { margin-left: 0.5em; padding-left: 0.8em; border-left: 3px solid #d2d2d7; color: #555; }
            </style>
            \(page)
            </head><body>\(body)</body></html>
            """
    }

    /// `<img>` tags with an http(s) or protocol-relative source, counted the
    /// way ui/src/lib/html.ts counts the images it blocks.
    static func remoteImageCount(_ html: String) -> Int {
        html.matches(of: /(?i)<img\b[^>]*?\ssrc\s*=\s*["']?\s*(https?:)?\/\//).count
    }

    static func linkified(_ text: String) -> AttributedString {
        var attributed = AttributedString(text)
        guard let detector = try? NSDataDetector(types: NSTextCheckingResult.CheckingType.link.rawValue) else { return attributed }
        for match in detector.matches(in: text, range: NSRange(text.startIndex..., in: text)) {
            guard let url = match.url, linkSchemes.contains(url.scheme?.lowercased() ?? ""), let range = Range(match.range, in: text),
                let attributedRange = Range(range, in: attributed)
            else { continue }
            attributed[attributedRange].link = url
        }
        return attributed
    }
}

/// An HTML email body. JavaScript is off, link clicks go to the environment's
/// `openURL` (the reader routes them through `AppModel.openLink`), every other
/// navigation is refused, and the view grows to fit its content
/// so the reader's ScrollView does the scrolling.
struct EmailWebView: View {
    let html: String
    /// Blocks every http(s) subresource (images, CSS, fonts) with a content rule list.
    var blockRemote = false
    let engine: Engine
    @State private var height: CGFloat = 40

    var body: some View {
        WebViewRepresentable(html: html, blockRemote: blockRemote, engine: engine, height: $height)
            .frame(height: height)
            .clipShape(RoundedRectangle(cornerRadius: 6))
    }
}

private struct WebViewRepresentable: NSViewRepresentable {
    let html: String
    let blockRemote: Bool
    let engine: Engine
    @Binding var height: CGFloat

    func makeCoordinator() -> Coordinator { Coordinator(height: $height) }

    func makeNSView(context: Context) -> FittingWebView {
        let config = WKWebViewConfiguration()
        // Cookies set by remote images would otherwise follow the user across every message.
        config.websiteDataStore = .nonPersistent()
        config.defaultWebpagePreferences.allowsContentJavaScript = false
        config.setURLSchemeHandler(AttachmentSchemeHandler(engine: engine), forURLScheme: EmailHTML.attachmentScheme)
        let view = FittingWebView(frame: .zero, configuration: config)
        view.setValue(false, forKey: "drawsBackground")
        view.navigationDelegate = context.coordinator
        view.onWidthChange = { [weak coordinator = context.coordinator, weak view] in
            if let view { coordinator?.measure(view) }
        }
        context.coordinator.openURL = context.environment.openURL
        context.coordinator.load(html, blockRemote: blockRemote, in: view)
        return view
    }

    func updateNSView(_ view: FittingWebView, context: Context) {
        context.coordinator.height = $height
        context.coordinator.openURL = context.environment.openURL
        context.coordinator.load(html, blockRemote: blockRemote, in: view)
    }

    @MainActor
    final class Coordinator: NSObject, WKNavigationDelegate {
        var height: Binding<CGFloat>
        var openURL: OpenURLAction?
        private var loaded: (html: String, blockRemote: Bool)?

        init(height: Binding<CGFloat>) { self.height = height }

        func load(_ html: String, blockRemote: Bool, in view: WKWebView) {
            guard loaded?.html != html || loaded?.blockRemote != blockRemote else { return }
            loaded = (html, blockRemote)
            Task {
                let rules = view.configuration.userContentController
                rules.removeAllContentRuleLists()
                if blockRemote {
                    // Fail closed: without the blocker the page would fetch what the user blocked.
                    guard let blocker = await RemoteContentBlocker.rules() else { return }
                    rules.add(blocker)
                }
                guard loaded?.html == html, loaded?.blockRemote == blockRemote else { return }
                view.loadHTMLString(html, baseURL: nil)
            }
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
                if EmailHTML.linkSchemes.contains(url.scheme?.lowercased() ?? "") { openURL?(url) }
                return .cancel
            }
            return url.absoluteString == "about:blank" ? .allow : .cancel
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            measure(webView)
        }
    }
}

@MainActor
private enum RemoteContentBlocker {
    private static var compiled: WKContentRuleList?

    static func rules() async -> WKContentRuleList? {
        if let compiled { return compiled }
        let json = #"[{"trigger":{"url-filter":"^https?://"},"action":{"type":"block"}}]"#
        do {
            compiled = try await WKContentRuleListStore.default().compileContentRuleList(
                forIdentifier: "mach.block-remote-content", encodedContentRuleList: json)
        } catch {
            NSLog("mach: remote content rule list failed to compile: \(error)")
        }
        return compiled
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

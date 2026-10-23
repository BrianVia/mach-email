import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct ComposerSheet: View {
    let model: AppModel
    @Bindable var composer: ComposerModel
    @FocusState private var focus: Field?
    @State private var importing = false
    @State private var pickingDate = false
    @State private var laterDate = Date.now.addingTimeInterval(3600)
    @State private var dropTargeted = false

    private enum Field { case to, cc, bcc, subject }

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            field("To", text: $composer.to, focus: .to)
            field("Cc", text: $composer.cc, focus: .cc)
            field("Bcc", text: $composer.bcc, focus: .bcc)
            field("Subject", text: $composer.subject, focus: .subject)
            ComposerBodyEditor(composer: composer, focusOnAppear: !composer.to.isEmpty)
                .overlay(alignment: .bottom) {
                    if !composer.snippetMatches.isEmpty { SnippetPicker(composer: composer) }
                }
            if !composer.attachments.isEmpty {
                Divider()
                attachments
            }
            Divider()
            footer
        }
        .frame(minWidth: 620, idealWidth: 720, minHeight: 480, idealHeight: 580)
        .defaultFocus($focus, composer.to.isEmpty ? .to : nil)
        .fileImporter(isPresented: $importing, allowedContentTypes: [.item], allowsMultipleSelection: true) { result in
            switch result {
            case .success(let urls): model.attach(urls, to: composer)
            case .failure(let error): model.show(error)
            }
        }
        .dropDestination(for: URL.self) { urls, _ in
            model.attach(urls.filter(\.isFileURL), to: composer)
            return true
        } isTargeted: { dropTargeted = $0 }
        .overlay {
            if dropTargeted {
                RoundedRectangle(cornerRadius: 8).strokeBorder(Color.accentColor, lineWidth: 3).padding(4).allowsHitTesting(false)
            }
        }
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline) {
            Text(composer.title).font(.headline)
            Spacer()
            Text("From").foregroundStyle(.secondary)
            Text(model.accountLabel(composer.draft.accountId))
                .help(composer.draft.accountId)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
    }

    private func field(_ label: String, text: Binding<String>, focus field: Field) -> some View {
        VStack(spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("\(label):")
                    .foregroundStyle(.secondary)
                    .frame(width: 58, alignment: .trailing)
                TextField("", text: text)
                    .textFieldStyle(.plain)
                    .accessibilityLabel(label)
                    .focused($focus, equals: field)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 7)
            Divider().padding(.leading, 16)
        }
    }

    private var attachments: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 6) {
                ForEach(composer.attachments, id: \.path) { attachment in
                    HStack(spacing: 4) {
                        Image(systemName: "paperclip").foregroundStyle(.secondary)
                        Text(attachment.filename).lineLimit(1)
                        Button {
                            composer.attachments.removeAll { $0.path == attachment.path }
                        } label: {
                            Image(systemName: "xmark.circle.fill").foregroundStyle(.secondary)
                        }
                        .buttonStyle(.plain)
                        .help("Remove \(attachment.filename)")
                    }
                    .font(.callout)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(.quaternary, in: Capsule())
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
        }
        .scrollIndicators(.never)
    }

    private var footer: some View {
        HStack(spacing: 10) {
            Button("Attach", systemImage: "paperclip") { importing = true }
                .help("Attach files, or drop them on the window")
            Button("Discard", systemImage: "trash") { Task { await model.discard(composer) } }
                .help("Delete this draft here and in Gmail")
            Spacer()
            Text("⌘S Save · Esc Close")
                .font(.caption)
                .foregroundStyle(.secondary)
            Menu("Send Later") {
                ForEach(composer.presets, id: \.self) { preset in
                    Button("\(preset.label) · \(preset.at.formatted(date: .abbreviated, time: .shortened))") {
                        Task { await model.sendLater(composer, at: preset.at) }
                    }
                }
                Divider()
                Button("Pick Date and Time…") { pickingDate = true }
            }
            .fixedSize()
            .popover(isPresented: $pickingDate, arrowEdge: .top) { laterPicker }
            Button {
                Task { do { try await model.send(composer) } catch { model.show(error) } }
            } label: {
                Label("Send", systemImage: "paperplane.fill")
            }
            .buttonStyle(.borderedProminent)
            .help("Send (⌘↩)")
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
    }

    private var laterPicker: some View {
        VStack(alignment: .trailing, spacing: 12) {
            DatePicker("Send at", selection: $laterDate, in: Date.now..., displayedComponents: [.date, .hourAndMinute])
                .datePickerStyle(.graphical)
                .labelsHidden()
            Button("Schedule Send") {
                pickingDate = false
                Task { await model.sendLater(composer, at: laterDate) }
            }
            .keyboardShortcut(.defaultAction)
        }
        .padding()
    }
}

private struct SnippetPicker: View {
    let composer: ComposerModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(composer.snippetMatches.enumerated()), id: \.element.name) { index, snippet in
                HStack(spacing: 12) {
                    Text(snippet.name).fontWeight(.medium)
                    Text(snippet.value.replacingOccurrences(of: "\n", with: " "))
                        .foregroundStyle(index == composer.snippetSelection ? .primary : .secondary)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .background(index == composer.snippetSelection ? Color.accentColor.opacity(0.2) : .clear, in: RoundedRectangle(cornerRadius: 5))
                .contentShape(Rectangle())
                .onTapGesture { composer.insertSnippet(at: index) }
            }
            Text("↩ or ⇥ to insert · Esc to dismiss")
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 10)
                .padding(.top, 4)
        }
        .padding(6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
        .shadow(color: .black.opacity(0.15), radius: 10, y: 3)
        .padding(12)
    }
}

/// Plain-text NSTextView: the body is Markdown, so smart quotes and dashes
/// stay off, and the delegate routes Return, Tab, and arrows to the snippet
/// picker while it is open.
private struct ComposerBodyEditor: NSViewRepresentable {
    let composer: ComposerModel
    let focusOnAppear: Bool

    func makeCoordinator() -> Coordinator { Coordinator(composer: composer) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        scroll.drawsBackground = false
        let text = scroll.documentView as! NSTextView
        text.delegate = context.coordinator
        text.font = .systemFont(ofSize: NSFont.systemFontSize + 1)
        text.textContainerInset = NSSize(width: 11, height: 10)
        text.isRichText = false
        text.allowsUndo = true
        text.drawsBackground = false
        text.isAutomaticQuoteSubstitutionEnabled = false
        text.isAutomaticDashSubstitutionEnabled = false
        text.isAutomaticTextReplacementEnabled = false
        text.string = composer.body
        composer.bodyView = text
        if focusOnAppear {
            DispatchQueue.main.async {
                text.window?.makeFirstResponder(text)
                text.setSelectedRange(NSRange(location: 0, length: 0))
                text.scrollRangeToVisible(NSRange(location: 0, length: 0))
            }
        }
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let text = scroll.documentView as? NSTextView, text.string != composer.body else { return }
        text.string = composer.body
    }

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        let composer: ComposerModel

        init(composer: ComposerModel) { self.composer = composer }

        func textDidChange(_ notification: Notification) {
            guard let text = notification.object as? NSTextView else { return }
            composer.body = text.string
            composer.updateSnippet(cursor: text.selectedRange().location)
        }

        func textView(_ textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            guard !composer.snippetMatches.isEmpty else { return false }
            switch selector {
            case #selector(NSResponder.insertNewline(_:)), #selector(NSResponder.insertTab(_:)): composer.insertSnippet()
            case #selector(NSResponder.moveDown(_:)): composer.moveSnippetSelection(by: 1)
            case #selector(NSResponder.moveUp(_:)): composer.moveSnippetSelection(by: -1)
            default: return false
            }
            return true
        }
    }
}

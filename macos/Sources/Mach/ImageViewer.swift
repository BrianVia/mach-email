import ImageIO
import SwiftUI

/// Full-window image attachment viewer. AppModel routes its keys
/// (Esc, ←/→, h/l, ⌘S) while it is open, so the keymap stays quiet.
struct ImageViewerView: View {
    let model: AppModel
    let viewer: ImageViewerState
    @State private var image: NSImage?
    @State private var failed = false

    var body: some View {
        ZStack {
            Color.black.opacity(0.88)
                .contentShape(Rectangle())
                .onTapGesture { model.closeViewer() }
            Group {
                if let image {
                    Image(nsImage: image).resizable().scaledToFit()
                        .clipShape(RoundedRectangle(cornerRadius: 4))
                } else if failed {
                    Label("Couldn't load \(viewer.current.filename)", systemImage: "exclamationmark.triangle")
                        .foregroundStyle(.white.opacity(0.7))
                } else {
                    ProgressView().controlSize(.large)
                }
            }
            .padding(.horizontal, 72)
            .padding(.top, 32)
            .padding(.bottom, 80)
            if viewer.images.count > 1 {
                HStack {
                    arrow("chevron.left", help: "Previous image (←)") { model.stepViewer(by: -1) }
                    Spacer()
                    arrow("chevron.right", help: "Next image (→)") { model.stepViewer(by: 1) }
                }
                .padding(.horizontal, 16)
            }
            VStack {
                Spacer()
                bar
            }
            .padding(.bottom, 20)
        }
        .environment(\.colorScheme, .dark)
        .task(id: viewer.current.attachmentId) { await load() }
    }

    private var bar: some View {
        HStack(spacing: 12) {
            Text(viewer.current.filename).lineLimit(1).truncationMode(.middle)
            Text("\(viewer.index + 1) / \(viewer.images.count)")
                .monospacedDigit()
                .foregroundStyle(.secondary)
            Button("Save", systemImage: "square.and.arrow.down") { model.saveViewedImage() }
                .help("Save to Downloads (⌘S)")
            Button("Close", systemImage: "xmark") { model.closeViewer() }
                .labelStyle(.iconOnly)
                .help("Close (Esc)")
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
        .padding(.leading, 16).padding(.trailing, 8).padding(.vertical, 6)
        .background(.ultraThinMaterial, in: Capsule())
        .frame(maxWidth: 560)
    }

    private func arrow(_ symbol: String, help: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 22, weight: .semibold))
                .frame(width: 44, height: 64)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(.white.opacity(0.85))
        .background(.white.opacity(0.1), in: RoundedRectangle(cornerRadius: 8))
        .help(help)
    }

    private func load() async {
        image = nil
        failed = false
        let attachment = viewer.current
        let data = try? await model.engine.attachment(
            account: viewer.message.accountId, messageId: viewer.message.id, attachmentId: attachment.attachmentId
        ).data
        guard !Task.isCancelled else { return }
        image = data.flatMap(NSImage.init(data:))
        failed = image == nil
    }
}

/// A downsampled square thumbnail; full-size photos would cost tens of MB each.
struct AttachmentThumbnail: View {
    let engine: Engine
    let message: Message
    let attachment: AttachmentRef
    @State private var image: NSImage?

    var body: some View {
        Color.secondary.opacity(0.12)
            .overlay {
                if let image {
                    Image(nsImage: image).resizable().scaledToFill()
                } else {
                    Image(systemName: "photo").font(.title2).foregroundStyle(.secondary)
                }
            }
            .aspectRatio(1, contentMode: .fit)
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .overlay { RoundedRectangle(cornerRadius: 8).strokeBorder(.separator) }
            .task(id: attachment.attachmentId) {
                guard let data = try? await engine.attachment(account: message.accountId, messageId: message.id, attachmentId: attachment.attachmentId).data,
                    let source = CGImageSourceCreateWithData(data as CFData, nil),
                    let cgImage = CGImageSourceCreateThumbnailAtIndex(source, 0, [
                        kCGImageSourceCreateThumbnailFromImageAlways: true,
                        kCGImageSourceCreateThumbnailWithTransform: true,
                        kCGImageSourceThumbnailMaxPixelSize: 400,
                    ] as CFDictionary)
                else { return }
                image = NSImage(cgImage: cgImage, size: .zero)
            }
    }
}

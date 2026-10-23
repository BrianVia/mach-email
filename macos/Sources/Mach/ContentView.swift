import SwiftUI

struct ContentView: View {
    @Bindable var model: AppModel

    var body: some View {
        NavigationSplitView {
            Sidebar(model: model)
                .navigationSplitViewColumnWidth(min: 180, ideal: 210, max: 280)
        } content: {
            ThreadList(model: model)
                .safeAreaInset(edge: .top, spacing: 0) { SystemBanners(model: model) }
                .navigationSplitViewColumnWidth(min: 320, ideal: 400)
        } detail: {
            detail
        }
        .navigationTitle(model.title)
        .navigationSubtitle(model.subtitle)
        .toolbar { HeaderToolbar(model: model) }
        .overlay {
            if let viewer = model.reader?.viewer { ImageViewerView(model: model, viewer: viewer) }
        }
        .overlay(alignment: .bottom) { Banner(model: model) }
        .frame(minWidth: 960, minHeight: 600)
        .sheet(item: Binding(get: { model.composer }, set: { if $0 == nil { model.escapeComposer() } })) { composer in
            ComposerSheet(model: model, composer: composer)
        }
    }

    @ViewBuilder private var detail: some View {
        switch model.composer?.background ?? model.route {
        case .thread(let reader):
            ReaderView(model: model, state: reader)
        case .activity:
            ContentUnavailableView("Activity", systemImage: "clock.arrow.circlepath", description: Text("Recent changes will appear here."))
        case .scheduled(let state):
            ScheduledView(model: model, state: state)
        default:
            ContentUnavailableView {
                Label("No Thread Open", systemImage: "envelope")
            } description: {
                Text("Press Return to open the selected thread.")
            }
        }
    }
}

private struct HeaderToolbar: ToolbarContent {
    @Bindable var model: AppModel

    var body: some ToolbarContent {
        ToolbarItemGroup(placement: .primaryAction) {
            if model.accounts.count > 1 {
                Menu {
                    Picker("Account", selection: Binding(get: { model.list.account }, set: { model.selectAccount($0) })) {
                        Text("All Accounts").tag(String?.none)
                        Divider()
                        ForEach(model.accounts, id: \.self) { email in
                            Text(model.accountLabel(email)).tag(Optional(email))
                        }
                    }
                    .pickerStyle(.inline)
                } label: {
                    Label(model.list.account.map(model.accountLabel) ?? "All Accounts", systemImage: "person.crop.circle")
                        .labelStyle(.titleAndIcon)
                }
                .help("Show one account")
                if model.list.account != nil {
                    Button("Show All Accounts", systemImage: "xmark.circle.fill") { model.selectAccount(nil) }
                        .labelStyle(.iconOnly)
                        .help("Show all accounts")
                }
            }
            StatusPill(connection: model.connection)
            if model.outbox.failed > 0 {
                Button {
                    Task { await model.retryFailedChanges() }
                } label: {
                    Label("\(model.outbox.failed) failed", systemImage: "exclamationmark.arrow.circlepath")
                        .labelStyle(.titleAndIcon)
                        .foregroundStyle(.red)
                }
                .help("\(model.outbox.lastError ?? "Some changes failed to sync.") Click to retry.")
            } else if model.outbox.pending > 0 {
                Label("\(model.outbox.pending) unsynced", systemImage: "arrow.triangle.2.circlepath")
                    .labelStyle(.titleAndIcon)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

private struct StatusPill: View {
    let connection: Connection

    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(color).frame(width: 7, height: 7)
            Text(title).font(.callout)
        }
        .padding(.horizontal, 8)
        .help(help)
    }

    private var color: Color {
        switch connection {
        case .live: .green
        case .connecting: .yellow
        case .offline: .secondary
        }
    }

    private var title: String {
        switch connection {
        case .live: "Live"
        case .connecting: "Connecting…"
        case .offline: "Offline"
        }
    }

    private var help: String {
        switch connection {
        case .live: "Every account synced on its last attempt"
        case .connecting: "Waiting for the first sync of every account"
        case .offline: "An account failed to sync or needs you to sign in again"
        }
    }
}

/// The transient notice and the chord hint, stacked at the bottom of the window.
private struct Banner: View {
    let model: AppModel

    var body: some View {
        VStack(spacing: 8) {
            if !model.chordBuffer.isEmpty {
                HStack(spacing: 12) {
                    Text(model.chordBuffer).font(.system(.body, design: .monospaced)).bold()
                    Image(systemName: "arrow.right").foregroundStyle(.secondary)
                    ForEach(model.continuations, id: \.self) { next in
                        HStack(spacing: 4) {
                            Text(next.next).font(.system(.callout, design: .monospaced)).padding(.horizontal, 5).padding(.vertical, 1)
                                .background(.quaternary, in: RoundedRectangle(cornerRadius: 4))
                            Text(next.actionName.replacingOccurrences(of: "_", with: " ")).font(.callout).foregroundStyle(.secondary)
                        }
                    }
                }
                .padding(.horizontal, 14).padding(.vertical, 8)
                .background(.regularMaterial, in: Capsule())
            }
            if let notice = model.notice {
                Label(notice.text, systemImage: notice.isError ? "exclamationmark.triangle.fill" : notice.path != nil ? "folder" : "info.circle")
                    .foregroundStyle(notice.isError ? AnyShapeStyle(.red) : AnyShapeStyle(.primary))
                    .lineLimit(2)
                    .padding(.horizontal, 14).padding(.vertical, 8)
                    .background(.regularMaterial, in: Capsule())
                    .onTapGesture { model.activateNotice() }
                    .help(notice.path == nil ? "" : "Show in Finder")
            }
        }
        .shadow(color: .black.opacity(0.12), radius: 8, y: 2)
        .padding(.bottom, 16)
        .animation(.snappy(duration: 0.2), value: model.notice)
        .animation(.snappy(duration: 0.2), value: model.chordBuffer)
    }
}

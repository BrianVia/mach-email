import SwiftUI

struct ContentView: View {
    @Bindable var model: AppModel

    var body: some View {
        NavigationSplitView {
            Sidebar(model: model)
                .navigationSplitViewColumnWidth(min: 180, ideal: 210, max: 280)
        } content: {
            Group {
                if let search = model.search { SearchView(model: model, state: search) } else { ThreadList(model: model) }
            }
            .navigationSplitViewColumnWidth(min: 320, ideal: 400)
        } detail: {
            detail
        }
        .navigationTitle(model.title)
        .navigationSubtitle(model.subtitle)
        .toolbar { HeaderToolbar(model: model) }
        .overlay(alignment: .bottom) { Banner(model: model) }
        .overlay(alignment: .top) {
            if let palette = model.palette {
                Color.black.opacity(0.08)
                    .ignoresSafeArea()
                    .onTapGesture { model.palette = nil }
                    .overlay(alignment: .top) { PaletteView(model: model, state: palette).padding(.top, 72) }
            }
        }
        .frame(minWidth: 960, minHeight: 600)
    }

    @ViewBuilder private var detail: some View {
        switch model.route {
        case .thread(let reader):
            ReaderView(model: model, state: reader)
        case .activity(let state):
            ActivityView(model: model, state: state)
        case .scheduled:
            ContentUnavailableView("Scheduled", systemImage: "clock", description: Text("Messages scheduled to send will appear here."))
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
            StatusPill(online: model.online)
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
    let online: Bool

    var body: some View {
        HStack(spacing: 5) {
            Circle().fill(online ? Color.green : Color.secondary).frame(width: 7, height: 7)
            Text(online ? "Live" : "Offline").font(.callout)
        }
        .padding(.horizontal, 8)
        .help(online ? "Every account synced on its last attempt" : "Waiting for every account to sync")
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
                Label(notice.text, systemImage: notice.isError ? "exclamationmark.triangle.fill" : "info.circle")
                    .foregroundStyle(notice.isError ? AnyShapeStyle(.red) : AnyShapeStyle(.primary))
                    .lineLimit(2)
                    .padding(.horizontal, 14).padding(.vertical, 8)
                    .background(.regularMaterial, in: Capsule())
                    .onTapGesture { model.show(nil) }
            }
        }
        .shadow(color: .black.opacity(0.12), radius: 8, y: 2)
        .padding(.bottom, 16)
        .animation(.snappy(duration: 0.2), value: model.notice)
        .animation(.snappy(duration: 0.2), value: model.chordBuffer)
    }
}

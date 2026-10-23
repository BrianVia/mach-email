import SwiftUI

struct Sidebar: View {
    @Bindable var model: AppModel
    @AppStorage("mach.labelsExpanded") private var labelsExpanded = true

    private enum Item: Hashable {
        case label(String)
        case activity
    }

    var body: some View {
        List(selection: selection) {
            Section("Mailboxes") {
                ForEach(AppModel.systemLabels, id: \.id) { label in
                    row(label.name, symbol: label.symbol, unread: label.id == "INBOX" ? model.systemUnread(label.id) : 0)
                        .tag(Item.label(label.id))
                }
            }
            if !model.userLabels.isEmpty {
                Section("Labels", isExpanded: $labelsExpanded) {
                    ForEach(model.userLabels) { label in
                        row(label.name, symbol: "tag", unread: label.unreadCount ?? 0)
                            .tag(Item.label(label.id))
                    }
                }
            }
            Section {
                Label("Activity", systemImage: "clock.arrow.circlepath").tag(Item.activity)
            }
        }
        .listStyle(.sidebar)
    }

    private func row(_ name: String, symbol: String, unread: Int) -> some View {
        Label(name, systemImage: symbol)
            .badge(unread)
    }

    private var selection: Binding<Item?> {
        Binding {
            switch model.route {
            case .activity: .activity
            case .scheduled: .label("SCHEDULED")
            default: .label(model.list.label)
            }
        } set: { item in
            switch item {
            case .activity: model.route = .activity
            case .label(let id): Task { do { try await model.openLabel(id) } catch { model.show(error) } }
            case nil: break
            }
        }
    }
}

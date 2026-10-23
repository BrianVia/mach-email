import SwiftUI

struct Sidebar: View {
    @Bindable var model: AppModel
    @AppStorage("mach.labelsExpanded") private var labelsExpanded = true
    /// Newline-separated label paths whose children are showing.
    @AppStorage("mach.labelGroupsExpanded") private var groupsExpanded = ""

    fileprivate enum Item: Hashable {
        case label(String)
        case activity
    }

    /// Changes only when the inbox badge could: another label, a read-state
    /// change in the list, or a sync. Comparing it is cheap, unlike recounting.
    private struct RecountKey: Equatable {
        var label: String
        var unread: Int
        var synced: Date?
    }

    var body: some View {
        List(selection: selection) {
            Section("Mailboxes") {
                ForEach(AppModel.systemLabels, id: \.id) { label in
                    Label(label.name, systemImage: label.symbol)
                        .badge(label.id == "INBOX" ? model.inboxUnreadCount : 0)
                        .tag(Item.label(label.id))
                }
                Label("Activity", systemImage: "clock.arrow.circlepath").tag(Item.activity)
            }
            if !model.userLabels.isEmpty {
                Section {
                    if labelsExpanded { LabelTree(nodes: LabelNode.tree(model.userLabels), expanded: $groupsExpanded) }
                } header: {
                    Button {
                        withAnimation(.snappy(duration: 0.2)) { labelsExpanded.toggle() }
                    } label: {
                        HStack {
                            Text("Labels")
                            Spacer()
                            Image(systemName: "chevron.right")
                                .rotationEffect(.degrees(labelsExpanded ? 90 : 0))
                                .imageScale(.small)
                        }
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help(labelsExpanded ? "Hide labels" : "Show labels")
                }
            }
        }
        .listStyle(.sidebar)
        .task(id: RecountKey(
            label: model.list.label,
            unread: model.list.threads.reduce(0) { $0 + ($1.unread ? 1 : 0) },
            synced: model.lastMailSyncedAt
        )) { await model.recountInbox() }
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
            case .activity: Task { do { try await model.openActivity() } catch { model.show(error) } }
            case .label(let id): Task { do { try await model.openLabel(id) } catch { model.show(error) } }
            case nil: break
            }
        }
    }
}

/// Gmail label paths ("Receipts/Amazon") folded into a tree. A parent Gmail
/// never created as its own label has no `label` and can't be opened.
struct LabelNode: Identifiable {
    var id: String
    var name: String
    var label: MailLabel?
    var children: [LabelNode] = []

    var unread: Int { label?.unreadCount ?? 0 }
    var totalUnread: Int { unread + children.reduce(0) { $0 + $1.totalUnread } }

    static func tree(_ labels: [MailLabel]) -> [LabelNode] {
        var root = LabelNode(id: "", name: "")
        for label in labels {
            root.insert(label, path: label.name.split(separator: "/").map(String.init), prefix: "")
        }
        return root.children
    }

    private mutating func insert(_ label: MailLabel, path: [String], prefix: String) {
        guard let head = path.first else { return }
        let id = prefix.isEmpty ? head : "\(prefix)/\(head)"
        if !children.contains(where: { $0.name == head }) { children.append(LabelNode(id: id, name: head)) }
        let index = children.firstIndex { $0.name == head }!
        if path.count == 1 { children[index].label = label } else { children[index].insert(label, path: Array(path.dropFirst()), prefix: id) }
    }
}

private struct LabelTree: View {
    let nodes: [LabelNode]
    @Binding var expanded: String

    var body: some View {
        ForEach(nodes) { node in
            if node.children.isEmpty {
                row(node, badge: node.unread)
            } else {
                let open = isExpanded(node.id)
                DisclosureGroup(isExpanded: binding(node.id)) {
                    LabelTree(nodes: node.children, expanded: $expanded)
                } label: {
                    // Collapsed, a parent counts its descendants' unread too, as Mail does.
                    row(node, badge: open ? node.unread : node.totalUnread)
                }
            }
        }
    }

    @ViewBuilder private func row(_ node: LabelNode, badge: Int) -> some View {
        let label = Label(node.name, systemImage: node.children.isEmpty ? "tag" : "folder").badge(badge).help(node.id)
        if let id = node.label?.id { label.tag(Sidebar.Item.label(id)) } else { label }
    }

    private func isExpanded(_ id: String) -> Bool { expanded.split(separator: "\n").contains { $0 == id } }

    private func binding(_ id: String) -> Binding<Bool> {
        Binding { isExpanded(id) } set: { open in
            var ids = Set(expanded.split(separator: "\n").map(String.init))
            if open { ids.insert(id) } else { ids.remove(id) }
            expanded = ids.sorted().joined(separator: "\n")
        }
    }
}

import SwiftUI

struct PaletteCommand: Identifiable {
    enum Perform {
        /// A keymap chord, resolved against the route under the palette when run.
        case chord(String)
        case openLabel(String)
        case account(String?)
        case retryOutbox
        case addAccount
        case notifications(Bool)
    }

    var title: String
    /// Shown as the shortcut; nil for commands with no key.
    var chord: String?
    var perform: Perform
    var id: String { title }
}

struct PaletteState {
    var commands: [PaletteCommand]
    var query = ""
    var selected = 0

    /// Prefix matches first, then word starts, label path segments ("arc"
    /// ranks Archive above "Go to Receipts/Archive"), substrings, and in-order
    /// letters ("mru" finds "Mark read, unread"); ties keep keymap order.
    var filtered: [PaletteCommand] {
        let needle = query.trimmingCharacters(in: .whitespaces).lowercased()
        guard !needle.isEmpty else { return commands }
        return commands.enumerated()
            .compactMap { index, command in Self.rank(command.title.lowercased(), needle).map { ($0, index, command) } }
            .sorted { ($0.0, $0.1) < ($1.0, $1.1) }
            .map(\.2)
    }

    var selectedCommand: PaletteCommand? {
        let visible = filtered
        return visible.isEmpty ? nil : visible[min(selected, visible.count - 1)]
    }

    static func rank(_ title: String, _ needle: String) -> Int? {
        if title.hasPrefix(needle) { return 0 }
        if title.contains(" " + needle) { return 1 }
        if title.contains("/" + needle) { return 2 }
        if title.contains(needle) { return 3 }
        var rest = Substring(needle)
        for character in title where character == rest.first { rest = rest.dropFirst() }
        return rest.isEmpty ? 4 : nil
    }
}

extension AppModel {
    private static let paletteTitles = [
        "archive": "Archive", "trash": "Move to Trash", "forward": "Forward", "compose_new": "Compose",
        "search": "Search", "open_thread": "Open Thread", "back_to_list": "Back to List", "undo": "Undo",
        "redo": "Redo", "refresh": "Sync Now", "mute": "Mute Thread", "unsubscribe": "Unsubscribe",
        "show_activity": "Show Activity", "load_older": "Load Older Mail", "add_account": "Add Account…",
        "inbox_split_important": "Important", "inbox_split_other": "Other", "inbox_split_updates": "Updates",
        "inbox_split_newsletters": "Newsletters", "save_draft": "Save Draft", "send_draft": "Send",
    ]
    private static let chordTitles = [
        "star shift+s": "Unstar", "star s": "Star", "mark_read shift+u": "Mark as Read", "mark_read u": "Mark as Unread",
        "reply r": "Reply", "reply shift+r": "Reply All", "respond_to_invite shift+a": "Accept Invite",
        "respond_to_invite shift+t": "Maybe Attend Invite", "respond_to_invite shift+d": "Decline Invite",
    ]

    /// Navigation first and removals last, so Return on an empty query never
    /// archives or trashes.
    private static func paletteGroup(_ name: String) -> Int {
        switch name {
        case "search", "open_label", "show_activity": 0
        case "compose_new", "reply", "forward": 1
        case "archive", "trash", "mute", "unsubscribe", "respond_to_invite": 3
        default: 2
        }
    }

    /// "Reply All" for `reply shift+r`, "Archive" for `archive e`.
    func actionTitle(name: String, chord: String) -> String {
        Self.chordTitles["\(name) \(chord)"] ?? Self.paletteTitles[name] ?? name.replacingOccurrences(of: "_", with: " ").capitalized
    }

    /// The label a `g` chord opens ("Inbox" for `g i`), or nil when the chord
    /// does something else.
    func destination(of chord: String, mode: KeyMode) -> String? {
        guard case .action(let action) = engine.resolveKey(mode: mode, chord: chord, context: keyContext),
            action.kind == "open_label", let label = action.fields.labelId
        else { return nil }
        let name = labelName(label)
        return name == label ? label.capitalized : name
    }

    /// What the Tauri palette lists: every binding in the current mode (the
    /// shortest chord wins per title), then the commands that have no key.
    func paletteCommands() -> [PaletteCommand] {
        let mode = route.mode
        var byTitle: [String: PaletteCommand] = [:]
        var order: [String] = []
        func add(_ command: PaletteCommand) {
            if let existing = byTitle[command.title] {
                if let new = command.chord, let old = existing.chord,
                    new.split(separator: " ").count < old.split(separator: " ").count
                {
                    byTitle[command.title] = command
                }
                return
            }
            byTitle[command.title] = command
            order.append(command.title)
        }
        let bindings = engine.keymapBindings(mode: mode).enumerated()
            .sorted { (Self.paletteGroup($0.element.name), $0.offset) < (Self.paletteGroup($1.element.name), $1.offset) }
            .map(\.element)
        for binding in bindings where !["select_next", "select_prev", "quit"].contains(binding.name) {
            let title = (binding.name == "open_label" ? destination(of: binding.chord, mode: mode) : nil).map { "Go to \($0)" }
                ?? actionTitle(name: binding.name, chord: binding.chord)
            add(PaletteCommand(title: title, chord: binding.chord, perform: .chord(binding.chord)))
        }
        for label in userLabels {
            add(PaletteCommand(title: "Go to \(label.name)", perform: .openLabel(label.id)))
        }
        if reader != nil { add(PaletteCommand(title: "Copy as Markdown", chord: "ctrl+shift+c", perform: .chord("ctrl+shift+c"))) }
        if case .list = route, accounts.count > 1 {
            for account in accounts where account != list.account {
                add(PaletteCommand(title: "Show Only \(accountLabel(account))", perform: .account(account)))
            }
            if list.account != nil { add(PaletteCommand(title: "Show All Accounts", perform: .account(nil))) }
        }
        add(PaletteCommand(title: "Retry Failed Changes", perform: .retryOutbox))
        add(PaletteCommand(title: "Add Account…", perform: .addAccount))
        add(PaletteCommand(title: "Notifications: On", perform: .notifications(true)))
        add(PaletteCommand(title: "Notifications: Off", perform: .notifications(false)))
        return order.compactMap { byTitle[$0] }
    }

    /// Keys the palette or the current route owns before the keymap: true
    /// consumes the key, false leaves it to the focused field, nil falls through.
    func handleRouteKey(_ chord: String) -> Bool? {
        if palette != nil { return handlePaletteKey(chord) }
        switch route {
        case .search: return handleSearchKey(chord)
        case .composer: return nil
        default:
            if chord == "ctrl+k" {
                togglePalette()
                return true
            }
        }
        if case .activity(let state) = route { return handleActivityKey(chord, state) }
        return nil
    }

    func togglePalette() {
        palette = palette == nil ? PaletteState(commands: paletteCommands()) : nil
        chordBuffer = ""
        continuations = []
    }

    /// Owns the keyboard while the palette is open: false sends the key to
    /// the filter field and never to the keymap.
    func handlePaletteKey(_ chord: String) -> Bool {
        guard let state = palette else { return false }
        switch chord {
        case "esc", "ctrl+k": palette = nil
        case "down", "up", "ctrl+n", "ctrl+p":
            let delta = chord == "down" || chord == "ctrl+n" ? 1 : -1
            palette?.selected = min(max(state.selected + delta, 0), max(state.filtered.count - 1, 0))
        case "enter":
            if let command = state.selectedCommand { Task { await runPaletteCommand(command) } }
        default: return false
        }
        return true
    }

    func runPaletteCommand(_ command: PaletteCommand) async {
        palette = nil
        switch command.perform {
        case .chord(let chord):
            if case .action(let action) = engine.resolveKey(mode: route.mode, chord: chord, context: keyContext) {
                await run(action)
            } else if !handleKey(chord) {
                show("\(command.title) isn't in the native app yet")
            }
        case .openLabel(let id):
            if let action = try? KeyAction(["kind": "open_label", "label_id": id]) { await run(action) }
        case .account(let account): selectAccount(account)
        case .retryOutbox: await retryFailedChanges()
        case .addAccount: await addAccount(loginHint: nil)
        case .notifications(let enabled):
            UserDefaults.standard.set(enabled, forKey: NotificationPreferences.enabledKey)
            show("Notifications \(enabled ? "on" : "off")")
        }
    }
}

/// Spotlight-style panel over the window; the route underneath stays live.
struct PaletteView: View {
    @Bindable var model: AppModel
    let state: PaletteState
    @FocusState private var fieldFocused: Bool
    @ScaledMetric private var width = 560.0

    var body: some View {
        let commands = state.filtered
        let selected = state.selectedCommand?.id
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Image(systemName: "command").font(.title3).foregroundStyle(.secondary)
                TextField("Command", text: query, prompt: Text("Type a command…"))
                    .textFieldStyle(.plain)
                    .font(.title3)
                    .focused($fieldFocused)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            Divider()
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(commands) { command in
                            PaletteRow(command: command, selected: command.id == selected)
                                .id(command.id)
                                .contentShape(Rectangle())
                                .onTapGesture { Task { await model.runPaletteCommand(command) } }
                        }
                    }
                    .padding(6)
                }
                .frame(maxHeight: 340)
                .fixedSize(horizontal: false, vertical: true)
                .onChange(of: selected) { _, id in
                    if let id { proxy.scrollTo(id) }
                }
            }
            .overlay {
                if commands.isEmpty {
                    Text("No matching commands").foregroundStyle(.secondary).padding()
                }
            }
        }
        .frame(width: width)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(.separator))
        .shadow(color: .black.opacity(0.25), radius: 24, y: 10)
        .onAppear { fieldFocused = true }
    }

    private var query: Binding<String> {
        Binding { model.palette?.query ?? "" } set: { text in
            model.palette?.query = text
            model.palette?.selected = 0
        }
    }
}

private struct PaletteRow: View {
    let command: PaletteCommand
    let selected: Bool

    var body: some View {
        HStack {
            Text(command.title)
            Spacer(minLength: 16)
            if let chord = command.chord {
                KeyCapsView(chord: chord)
                    .foregroundStyle(selected ? AnyShapeStyle(.white.opacity(0.9)) : AnyShapeStyle(.secondary))
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .foregroundStyle(selected ? AnyShapeStyle(.white) : AnyShapeStyle(.primary))
        .background(selected ? AnyShapeStyle(Color.accentColor) : AnyShapeStyle(.clear), in: RoundedRectangle(cornerRadius: 6))
    }
}

/// Keymap chords as Mac key caps: "ctrl+shift+c" is "⇧⌘C", "g i" is two caps.
enum KeyCaps {
    private static let symbols = [
        "ctrl": "⌘", "shift": "⇧", "alt": "⌥", "enter": "↩", "esc": "⎋", "tab": "⇥", "space": "Space",
        "backspace": "⌫", "delete": "⌦", "up": "↑", "down": "↓", "left": "←", "right": "→",
    ]
    private static let modifierOrder = ["alt", "shift", "ctrl"]

    static func of(_ chord: String) -> [String] {
        chord.split(separator: " ").map { step in
            let parts = step.split(separator: "+").map(String.init)
            let modifiers = modifierOrder.filter(parts.dropLast().contains).map { symbols[$0]! }
            let key = parts.last ?? ""
            return (modifiers + [symbols[key] ?? key.uppercased()]).joined()
        }
    }
}

/// A chord drawn as key caps, shared by the palette and the chord hint.
struct KeyCapsView: View {
    let chord: String

    var body: some View {
        HStack(spacing: 3) {
            ForEach(Array(KeyCaps.of(chord).enumerated()), id: \.offset) { _, cap in
                Text(cap)
                    .font(.system(.caption, design: .rounded).weight(.medium))
                    .frame(minWidth: 18)
                    .padding(.horizontal, 4)
                    .padding(.vertical, 1)
                    .background(.quaternary, in: RoundedRectangle(cornerRadius: 4))
            }
        }
    }
}

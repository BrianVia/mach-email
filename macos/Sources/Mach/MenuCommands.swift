import AppKit
import SwiftUI

/// The menu bar. Items build the same actions the keymap does and go
/// through `AppModel.run`. A shortcut whose chord the keymap binds (⌘R,
/// ⌘N, ⌘K) is consumed by the key monitor before the menu sees it, so it
/// never fires twice; the others are chords the keymap leaves unbound.
struct MenuCommands: Commands {
    let model: AppModel?

    var body: some Commands {
        CommandGroup(replacing: .newItem) {
            Button("New Message") { perform("compose_new") }
            Button("Add Account…") { Task { await model?.addAccount() } }
                .keyboardShortcut("n")
        }
        CommandGroup(replacing: .undoRedo) {
            Button("Undo") { undoOrRedo("undo") }.keyboardShortcut("z")
            Button("Redo") { undoOrRedo("redo") }.keyboardShortcut("z", modifiers: [.command, .shift])
        }
        CommandGroup(before: .sidebar) {
            Button("Command Palette…") { model?.togglePalette() }.keyboardShortcut("k")
            Button("Search Mail") { model?.openSearch() }.keyboardShortcut("f", modifiers: [.command, .option])
            Button("Activity") { Task { do { try await model?.openActivity() } catch { model?.show(error) } } }
                .keyboardShortcut("0", modifiers: [.command, .option])
            Divider()
            ForEach(Array(AppModel.systemLabels.enumerated()), id: \.offset) { index, label in
                Button(label.name) { perform("open_label", ["label_id": label.id]) }
                    .keyboardShortcut(index < 9 ? KeyboardShortcut(KeyEquivalent(Character("\(index + 1)"))) : nil)
            }
            Divider()
            ForEach(Split.allCases) { split in
                Button(split.title) { perform("inbox_split_\(split.rawValue)") }
            }
            Divider()
        }
        // No ⇧⌘-letter equivalents: AppKit matched ⌘R to a ⇧⌘R item while a
        // text field had focus, so ⌘R in the search field ran Reply All.
        CommandMenu("Message") {
            Button("Reply") { perform("reply", ["all": false]) }
            Button("Reply All") { perform("reply", ["all": true]) }
            Button("Forward") { perform("forward") }
            Divider()
            Button("Archive") { perform("archive") }.keyboardShortcut("a", modifiers: [.command, .control])
            // No ⌘⌫: menu equivalents beat the focused field, where it deletes a line.
            Button("Move to Trash") { perform("trash") }
            Button("Mute Thread") { perform("mute") }
            Divider()
            Button("Star") { perform("star", ["starred": true]) }
            Button("Unstar") { perform("star", ["starred": false]) }
            Button("Mark as Read") { perform("mark_read", ["read": true]) }
            Button("Mark as Unread") { perform("mark_read", ["read": false]) }
        }
        CommandMenu("Mailbox") {
            Button("Sync Now") { perform("refresh") }.keyboardShortcut("r")
            Button("Retry Failed Changes") { Task { await model?.retryFailedChanges() } }
            Button("Load Older Mail") { perform("load_older") }
        }
    }

    /// Fills the selection fields the keymap's `$selection` and
    /// `$current_message` macros would.
    private func perform(_ kind: String, _ fields: [String: Any] = [:]) {
        guard let model else { return }
        let context = model.keyContext
        var object = fields
        object["kind"] = kind
        if ["archive", "trash", "mute", "star", "mark_read"].contains(kind) { object["thread_ids"] = context.selection }
        if ["reply", "forward"].contains(kind) { object["message_id"] = context.currentMessage }
        guard let action = try? KeyAction(object) else { return }
        Task { await model.run(action) }
    }

    /// Text fields keep their own undo; everywhere else it is mail undo.
    private func undoOrRedo(_ kind: String) {
        if let editor = NSApp.keyWindow?.firstResponder as? NSTextView, editor.isEditable {
            NSApp.sendAction(NSSelectorFromString("\(kind):"), to: nil, from: nil)
        } else {
            perform(kind)
        }
    }
}

import AppKit

/// Translates NSEvents to the keymap's chord strings the way
/// ui/src/lib/keymap.ts `keyEventToChord` does: cmd and ctrl both become
/// "ctrl", shift is spelled out only for letters, and "?" is "shift+/" as
/// config/default-keymap.toml writes it.
@MainActor
enum KeyChord {
    private static let specials: [(name: String, code: UInt16, chars: String)] = [
        ("enter", 36, "\r"),
        ("esc", 53, "\u{1b}"),
        ("tab", 48, "\t"),
        ("space", 49, " "),
        ("backspace", 51, "\u{7f}"),
        ("delete", 117, String(UnicodeScalar(UInt16(NSDeleteFunctionKey))!)),
        ("left", 123, String(UnicodeScalar(UInt16(NSLeftArrowFunctionKey))!)),
        ("right", 124, String(UnicodeScalar(UInt16(NSRightArrowFunctionKey))!)),
        ("down", 125, String(UnicodeScalar(UInt16(NSDownArrowFunctionKey))!)),
        ("up", 126, String(UnicodeScalar(UInt16(NSUpArrowFunctionKey))!)),
    ]

    /// nil for bare modifier presses and dead keys.
    static func chord(for event: NSEvent) -> String? {
        let flags = event.modifierFlags
        var parts: [String] = []
        if flags.contains(.control) || flags.contains(.command) { parts.append("ctrl") }
        if flags.contains(.option) { parts.append("alt") }
        let key: String
        if let special = specials.first(where: { $0.code == event.keyCode && event.charactersIgnoringModifiers == $0.chars }) {
            key = special.name
        } else {
            guard let chars = event.charactersIgnoringModifiers, chars.count == 1 else { return nil }
            let lower = chars.lowercased()
            if flags.contains(.shift) && lower != chars { parts.append("shift") }
            if lower == "?" {
                parts.append("shift")
                key = "/"
            } else {
                key = lower
            }
        }
        return (parts + [key]).joined(separator: "+")
    }

    /// ANSI key codes, so synthesized events match menu key equivalents the way real keys do.
    private static let letterCodes: [Character: UInt16] = Dictionary(uniqueKeysWithValues: zip(
        "asdfhgzxcv bqweryt123465=97-80]ou[ip lj'k;\\,/nm.", (0...47).map(UInt16.init)
    ).filter { $0.0 != " " })

    /// Synthesizes the keyDown a real keyboard would send, for the debug server.
    static func event(key: String, modifiers held: Set<String>, window: NSWindow?) -> NSEvent? {
        let special = specials.first { $0.name == key }
        var chars = special?.chars ?? key
        if held.contains("shift"), special == nil { chars = chars.uppercased() }
        let flags: [(String, NSEvent.ModifierFlags)] = [("cmd", .command), ("ctrl", .control), ("alt", .option), ("shift", .shift)]
        return NSEvent.keyEvent(
            with: .keyDown, location: .zero,
            modifierFlags: NSEvent.ModifierFlags(flags.filter { held.contains($0.0) }.map(\.1)),
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window?.windowNumber ?? 0,
            context: nil, characters: chars, charactersIgnoringModifiers: chars, isARepeat: false,
            keyCode: special?.code ?? key.lowercased().first.flatMap { letterCodes[$0] } ?? 0)
    }
}

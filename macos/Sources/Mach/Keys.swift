import AppKit

/// Translates between NSEvents and keymap chord names like "j", "esc", "cmd+k".
@MainActor
enum KeyChord {
    private static let specials: [String: (code: UInt16, chars: String)] = [
        "enter": (36, "\r"),
        "esc": (53, "\u{1b}"),
        "down": (125, String(Character(Unicode.Scalar(UInt32(NSDownArrowFunctionKey))!))),
        "up": (126, String(Character(Unicode.Scalar(UInt32(NSUpArrowFunctionKey))!))),
    ]

    private static let modifiers: [(name: String, flag: NSEvent.ModifierFlags)] = [
        ("cmd", .command), ("ctrl", .control), ("alt", .option), ("shift", .shift),
    ]

    static func name(of event: NSEvent) -> String {
        let key = specials.first { $0.value.code == event.keyCode }?.key
            ?? (event.charactersIgnoringModifiers ?? "").lowercased()
        let held = modifiers.filter { event.modifierFlags.contains($0.flag) }.map(\.name)
        return (held + [key]).joined(separator: "+")
    }

    static func event(key: String, modifiers held: Set<String>, window: NSWindow?) -> NSEvent? {
        let special = specials[key]
        let chars = special?.chars ?? key
        let flags = NSEvent.ModifierFlags(modifiers.filter { held.contains($0.name) }.map(\.flag))
        return NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: flags,
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window?.windowNumber ?? 0,
            context: nil, characters: chars, charactersIgnoringModifiers: chars, isARepeat: false,
            keyCode: special?.code ?? 0)
    }
}

# Mach for macOS

Native SwiftUI shell for mach. See [docs/macos-native.md](../docs/macos-native.md)
for the architecture and the parity checklist. The app runs on the Rust engine
through `crates/mach-ffi` and shares the Tauri app's data directory.

Code map: `Models.swift` mirrors the bridge's JSON, `Engine.swift` wraps
`MachEngine` with typed async calls and an event stream, `AppModel.swift` holds
all UI state (`Route`, `ListState`, keymap dispatch), and the views are
`Sidebar`, `ThreadList`, `Reader`, and `EmailWebView`.

## Build

`build-app.sh` runs `build-ffi.sh` first whenever `Generated/` is missing or
older than any Rust source.

```sh
macos/scripts/build-app.sh            # macos/build/Mach.app, ad-hoc signed
macos/scripts/build-app.sh --install  # also copies it to ~/Applications/Mach-dev/
open ~/Applications/Mach-dev/Mach.app
```

The build treats warnings as errors. Ad-hoc signing (`codesign -s -`) works over
SSH, where the login keychain is locked.

## Debug server

The app listens on `127.0.0.1:$MACH_DEBUG_PORT` (default 7366). `open` does not
pass environment variables, so use `launchctl setenv MACH_DEBUG_PORT 7400`
before launching to change the port.

```sh
curl 127.0.0.1:7366/health
curl 127.0.0.1:7366/state                                   # route, label, account filter, split, selection, opened thread, notice
curl -X POST 127.0.0.1:7366/key -d '{"key":"j"}'            # synthesized NSEvent keyDown; replies with /state
curl -X POST 127.0.0.1:7366/key -d '{"key":"k","cmd":true,"shift":false,"alt":false,"ctrl":false}'
curl -X POST 127.0.0.1:7366/action -d '{"name":"open_thread"}'               # keymap action; fields default to the selection
curl -X POST 127.0.0.1:7366/action -d '{"name":"open_label","label_id":"STARRED"}'
curl -X POST 127.0.0.1:7366/action -d '{"name":"account","account":null}'     # account filter
curl -X POST 127.0.0.1:7366/action -d '{"name":"appearance","value":"dark"}'  # light, dark, or system
curl -o mach.png 127.0.0.1:7366/snapshot                    # PNG of the app's own window, no screen-recording permission
curl -X POST 127.0.0.1:7366/notify -d '{"title":"Ada","body":"Hi","thread_id":"inbox-2"}'
```

Keys go through the same keymap path as the keyboard (`config/default-keymap.toml`
resolved by the engine). Key names are `enter`, `esc`, `tab`, `space`,
`backspace`, `delete`, arrows, or any single character; `{"key":"r","cmd":true}`
is `ctrl+r`. Unknown action names return 400 with the list of known ones.

`/state` reports `notifications.authorization` (macOS's answer) and
`notifications.request` (`awaiting_user`, `allowed`, or `refused`, from the
app's own `requestAuthorization` call). Clicking a notification sets
`opened_thread_id` to its `thread_id`. Activating the window never opens a thread.

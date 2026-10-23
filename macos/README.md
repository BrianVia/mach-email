# Mach for macOS

Native SwiftUI shell for mach. See [docs/macos-native.md](../docs/macos-native.md)
for the architecture and the parity checklist. The UI runs on `PreviewEngine`
sample data until the Rust bridge (`crates/mach-ffi`) lands.

## Build

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
curl 127.0.0.1:7366/state                                   # view, selection, window title, notification settings
curl -X POST 127.0.0.1:7366/key -d '{"key":"j"}'            # synthesized NSEvent keyDown; replies with /state
curl -X POST 127.0.0.1:7366/key -d '{"key":"k","cmd":true,"shift":false,"alt":false,"ctrl":false}'
curl -X POST 127.0.0.1:7366/action -d '{"name":"open_thread"}'
curl -o mach.png 127.0.0.1:7366/snapshot                    # PNG of the app's own window, no screen-recording permission
curl -X POST 127.0.0.1:7366/notify -d '{"title":"Ada","body":"Hi","thread_id":"inbox-2"}'
```

Key names are `j`, `k`, `l`, `enter`, `esc`, `up`, `down`, or any single
character. Unknown action names return 400 with the list of known ones.

`/state` reports `notifications.authorization` (macOS's answer) and
`notifications.request` (`awaiting_user`, `allowed`, or `refused`, from the
app's own `requestAuthorization` call). Clicking a notification sets
`opened_thread_id` to its `thread_id`. Activating the window never opens a thread.

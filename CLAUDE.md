# Debugging the macOS app (for agents)

The desktop app is the native SwiftUI app in `macos/` (bundle id `com.via.mach`).
See [docs/agent-debugging.md](docs/agent-debugging.md) for the full guide and
[docs/macos-native.md](docs/macos-native.md) for the standing orders.

```sh
tail -f ~/Library/Application\ Support/com.via.mach/native.log
MACH_DEBUG_PORT=7366 ~/Applications/Mach-dev/Mach.app/Contents/MacOS/Mach
curl 127.0.0.1:7366/state
curl -X POST -H 'X-Mach-Debug: 1' 127.0.0.1:7366/key -d '{"key":"esc"}'
curl -X POST -H 'X-Mach-Debug: 1' 127.0.0.1:7366/action -d '{"name":"open_thread"}'
curl -o mach.png 127.0.0.1:7366/snapshot
```

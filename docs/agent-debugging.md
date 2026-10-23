# Agent debugging

The native macOS app (`macos/`) runs a debug server only when `MACH_DEBUG_PORT`
is set. It binds `127.0.0.1`, so agents drive it over SSH on the Mac. Build a
dev copy into `~/Applications/Mach-dev/` and launch the binary directly, since
`open` does not pass environment variables:

```sh
bash macos/scripts/build-app.sh --install
MACH_DEBUG_PORT=7366 ~/Applications/Mach-dev/Mach.app/Contents/MacOS/Mach
```

Never launch, replace, or quit `/Applications/Mach.app`; it is the owner's
daily app. A dev build shares its data directory, so follow the standing orders
in [macos-native.md](macos-native.md) about real mail.

Every POST must carry `X-Mach-Debug: 1`, or the server answers 403.

```sh
curl 127.0.0.1:7366/health
curl 127.0.0.1:7366/state                                                    # route, selection, opened thread, system status
curl -X POST -H 'X-Mach-Debug: 1' 127.0.0.1:7366/key -d '{"key":"j"}'          # through the keymap, like a real keypress
curl -X POST -H 'X-Mach-Debug: 1' 127.0.0.1:7366/action -d '{"name":"open_thread"}'
curl -o mach.png 127.0.0.1:7366/snapshot                                     # PNG of the app window
```

`screencapture` cannot run from an SSH session, so `/snapshot` is how to see
the UI. Unknown action names return 400 with the list of known ones. The full
list of actions and key names is in [macos/README.md](../macos/README.md#debug-server).

## Log

The engine logs to `native.log` in the data directory, truncated at each
launch, so starting a dev build clears the log of a running copy.

```sh
tail -f ~/Library/Application\ Support/com.via.mach/native.log
RUST_LOG=mach=debug MACH_DEBUG_PORT=7366 ~/Applications/Mach-dev/Mach.app/Contents/MacOS/Mach
```

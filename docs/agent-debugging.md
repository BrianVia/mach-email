# Agent debugging

The debug sidecar is disabled by default. Start the installed app with a loopback-only HTTP server:

```sh
MACH_DEBUG_PORT=7365 ~/.local/bin/mach-desktop
```

All endpoints return JSON:

```sh
curl http://127.0.0.1:7365/health
curl http://127.0.0.1:7365/state
curl http://127.0.0.1:7365/dom
curl -X POST http://127.0.0.1:7365/eval -d '{"js":"document.title"}'
curl -X POST http://127.0.0.1:7365/key -d '{"key":"Escape","ctrl":false,"shift":false,"alt":false,"meta":false}'
```

`/eval` accepts async expressions too:

```sh
curl -X POST http://127.0.0.1:7365/eval -d '{"js":"await Promise.resolve(location.href)"}'
```

Frontend warnings, errors, uncaught errors, and slow IPC calls are forwarded to the desktop log. The log is at `~/.local/share/mach/desktop.log` on Linux and `~/Library/Application Support/com.via.mach/desktop.log` on macOS. Follow it with:

```sh
tail -f ~/.local/share/mach/desktop.log
```

Calls slower than 100ms are logged automatically. To log every IPC call (never its arguments), run this through `/eval` or the inspector:

```js
localStorage.setItem("mach.debugIpc", "1")
```

Disable it with `localStorage.removeItem("mach.debugIpc")`.

For the remote WebKit inspector on Linux, launch the devtools-enabled binary with:

```sh
WEBKIT_INSPECTOR_SERVER=127.0.0.1:9223 MACH_DEBUG_PORT=7365 ~/.local/bin/mach-desktop
```

Then connect a WebKit-compatible inspector to `127.0.0.1:9223` for profiling.

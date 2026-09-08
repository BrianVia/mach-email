# Debugging the desktop app (for agents)

See [docs/agent-debugging.md](docs/agent-debugging.md) for the full sidecar and WebKit inspector guide.

```sh
tail -f ~/.local/share/mach/desktop.log
MACH_DEBUG_PORT=7365 ~/.local/bin/mach-desktop
curl http://127.0.0.1:7365/state
curl -X POST http://127.0.0.1:7365/key -d '{"key":"Escape","ctrl":false,"shift":false,"alt":false,"meta":false}'
curl -X POST http://127.0.0.1:7365/eval -d '{"js":"document.title"}'
```

/* @refresh reload */
import { render } from "solid-js/web";
import App from "./App";
import "./styles.css";

// Global error trap. Renders the message as a fullscreen overlay so it's
// impossible to miss even when SolidJS itself failed to mount.
function paintError(msg: string) {
  // Style the host body so we don't depend on Tailwind CSS having loaded.
  document.documentElement.style.background = "#0a0d12";
  document.body.style.cssText =
    "margin:0;padding:0;background:#0a0d12;color:#ff7066;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;";
  const overlay = document.createElement("div");
  overlay.style.cssText = [
    "position:fixed",
    "inset:0",
    "padding:24px",
    "background:#0a0d12",
    "color:#ff7066",
    "font-size:14px",
    "line-height:1.45",
    "white-space:pre-wrap",
    "word-break:break-all",
    "overflow-wrap:anywhere",
    "overflow:auto",
    "z-index:2147483647",
    "box-sizing:border-box",
  ].join(";");
  overlay.textContent = msg;
  document.body.appendChild(overlay);
}

window.addEventListener("error", (e) => {
  paintError(`window.onerror: ${e.message}\n${e.filename}:${e.lineno}:${e.colno}\n${e.error?.stack ?? ""}`);
});
window.addEventListener("unhandledrejection", (e) => {
  paintError(`unhandledrejection: ${e.reason?.message ?? e.reason}\n${e.reason?.stack ?? ""}`);
});

try {
  const root = document.getElementById("app");
  if (!root) throw new Error("missing #app");
  // Clear the boot probe (the inline-styled "mach is booting…" div from index.html).
  root.innerHTML = "";
  render(() => <App />, root);
} catch (e) {
  paintError(`render() threw: ${(e as Error).message}\n${(e as Error).stack ?? ""}`);
}


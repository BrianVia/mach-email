import { frontendLog, isInTauri } from "./ipc";

export function stringifyArgs(args: unknown[], maxLength = 2000): string {
  return args.map((arg) => {
    try {
      if (typeof arg === "string") return arg;
      if (arg instanceof Error) return arg.stack ?? arg.message;
      return JSON.stringify(arg) ?? String(arg);
    } catch {
      try {
        return String(arg);
      } catch {
        return "[unprintable]";
      }
    }
  }).join(" ").slice(0, maxLength);
}

if (isInTauri()) {
  let forwarding = false;
  const forward = (level: "error" | "warn", args: unknown[]) => {
    if (forwarding) return;
    forwarding = true;
    try {
      frontendLog(level, stringifyArgs(args));
    } finally {
      forwarding = false;
    }
  };
  const error = console.error.bind(console);
  const warn = console.warn.bind(console);
  console.error = (...args) => {
    error(...args);
    forward("error", args);
  };
  console.warn = (...args) => {
    warn(...args);
    forward("warn", args);
  };
  window.addEventListener("error", (event) => {
    forward("error", [event.message, event.filename, event.lineno, event.colno, event.error]);
  });
  window.addEventListener("unhandledrejection", (event) => {
    forward("error", ["unhandledrejection", event.reason]);
  });
}

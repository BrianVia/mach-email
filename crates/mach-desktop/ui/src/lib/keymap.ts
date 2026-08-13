// Frontend keymap resolver. Same TOML format as the Rust side; we use
// `smol-toml` to parse it. Macros are substituted at dispatch time.

import { parse as parseToml } from "smol-toml";

export type Mode = "normal" | "reading" | "composing" | "search" | "filter";

export type KeyContext = {
  selection: string[];
  current_thread?: string | null;
  current_message?: string | null;
  current_draft?: string | null;
};

export type Resolution =
  | { kind: "action"; action: Record<string, unknown> }
  | { kind: "prefix"; continuations: { next: string; action_name: string }[] }
  | { kind: "unbound" };

type Template = string | Record<string, unknown>;

export class Keymap {
  private modes = new Map<Mode, Map<string, Template>>();
  private prefixes = new Map<Mode, Set<string>>();

  static fromToml(toml: string): Keymap {
    const km = new Keymap();
    const parsed = parseToml(toml) as Record<string, Record<string, Template>>;
    for (const [modeName, bindings] of Object.entries(parsed)) {
      const mode = modeName as Mode;
      if (!km.modes.has(mode)) km.modes.set(mode, new Map());
      const m = km.modes.get(mode)!;
      for (const [chord, tpl] of Object.entries(bindings)) {
        m.set(chord, tpl);
      }
    }
    km.rebuildPrefixes();
    return km;
  }

  merge(overrides: Keymap) {
    for (const [mode, bindings] of overrides.modes.entries()) {
      if (!this.modes.has(mode)) this.modes.set(mode, new Map());
      const current = this.modes.get(mode)!;
      for (const [chord, template] of bindings.entries()) {
        current.set(chord, template);
      }
    }
    this.rebuildPrefixes();
  }

  bindings(mode: Mode): { chord: string; name: string }[] {
    return Array.from(this.modes.get(mode)?.entries() ?? [], ([chord, tpl]) => ({
      chord,
      name: templateActionName(tpl),
    }));
  }

  private rebuildPrefixes() {
    this.prefixes.clear();
    for (const [mode, m] of this.modes.entries()) {
      const set = new Set<string>();
      for (const chord of m.keys()) {
        const parts = chord.split(/\s+/);
        for (let i = 1; i < parts.length; i++) {
          set.add(parts.slice(0, i).join(" "));
        }
      }
      this.prefixes.set(mode, set);
    }
  }

  resolve(mode: Mode, chord: string, ctx: KeyContext): Resolution {
    const m = this.modes.get(mode);
    if (!m) return { kind: "unbound" };
    const tpl = m.get(chord);
    if (tpl !== undefined) {
      try {
        const action = resolveTemplate(tpl, ctx);
        return { kind: "action", action };
      } catch {
        return { kind: "unbound" };
      }
    }
    if (this.prefixes.get(mode)?.has(chord)) {
      const conts: { next: string; action_name: string }[] = [];
      const pfx = chord + " ";
      for (const [c, t] of m.entries()) {
        if (c.startsWith(pfx)) {
          const rest = c.slice(pfx.length);
          const next = rest.split(/\s+/)[0];
          const action_name = templateActionName(t);
          conts.push({ next, action_name });
        }
      }
      conts.sort((a, b) => a.next.localeCompare(b.next));
      // de-dupe by `next`
      const seen = new Set<string>();
      const unique = conts.filter((c) =>
        seen.has(c.next) ? false : (seen.add(c.next), true),
      );
      return { kind: "prefix", continuations: unique };
    }
    return { kind: "unbound" };
  }
}

function templateActionName(t: Template): string {
  if (typeof t === "string") return t;
  return (t.kind as string) ?? "<?>";
}

function resolveTemplate(t: Template, ctx: KeyContext): Record<string, unknown> {
  if (typeof t === "string") {
    return { kind: t };
  }
  return substitute(t, ctx) as Record<string, unknown>;
}

function substitute(v: unknown, ctx: KeyContext): unknown {
  if (typeof v === "string" && v.startsWith("$")) {
    switch (v) {
      case "$selection":
        return ctx.selection ?? [];
      case "$current_thread":
        if (!ctx.current_thread) throw new Error("missing $current_thread");
        return ctx.current_thread;
      case "$current_message":
        if (!ctx.current_message) throw new Error("missing $current_message");
        return ctx.current_message;
      case "$current_draft":
        if (!ctx.current_draft) throw new Error("missing $current_draft");
        return ctx.current_draft;
      default:
        throw new Error(`unknown macro: ${v}`);
    }
  }
  if (Array.isArray(v)) return v.map((x) => substitute(x, ctx));
  if (v && typeof v === "object") {
    const out: Record<string, unknown> = {};
    for (const [k, val] of Object.entries(v)) out[k] = substitute(val, ctx);
    return out;
  }
  return v;
}

/**
 * Convert a DOM KeyboardEvent into the chord string used by the keymap.
 * Mirrors the Rust crossterm normalization.
 */
export function keyEventToChord(e: KeyboardEvent): string | null {
  const parts: string[] = [];
  if (e.ctrlKey || e.metaKey) parts.push("ctrl"); // map cmd → ctrl for Mac feel
  if (e.altKey) parts.push("alt");
  // Shift is folded into uppercase letter; we re-add explicit prefix for clarity.
  let key: string;
  switch (e.key) {
    case "Enter":
      key = "enter";
      break;
    case "Escape":
      key = "esc";
      break;
    case "Tab":
      key = "tab";
      break;
    case "Backspace":
      key = "backspace";
      break;
    case "Delete":
      key = "delete";
      break;
    case "ArrowUp":
      key = "up";
      break;
    case "ArrowDown":
      key = "down";
      break;
    case "ArrowLeft":
      key = "left";
      break;
    case "ArrowRight":
      key = "right";
      break;
    case " ":
      key = "space";
      break;
    case "Shift":
    case "Control":
    case "Alt":
    case "Meta":
      return null;
    default:
      if (e.key.length === 1) {
        const lower = e.key.toLowerCase();
        if (e.shiftKey && lower !== e.key) parts.push("shift");
        key = lower;
      } else {
        key = e.key.toLowerCase();
      }
  }
  return parts.length ? `${parts.join("+")}+${key}` : key;
}

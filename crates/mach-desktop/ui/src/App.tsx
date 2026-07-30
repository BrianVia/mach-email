import { createSignal, createResource, Show, onMount, onCleanup } from "solid-js";
import { Keymap, keyEventToChord, type KeyContext, type Mode } from "./lib/keymap";
import {
  dispatchAction,
  fetchAccountStatus,
  fetchKeymapSources,
  listThreads,
  openThread as openThreadIpc,
  refetchThread as refetchThreadIpc,
  searchThreads,
  type Message,
  type ThreadSummary,
} from "./lib/ipc";
import Inbox from "./views/Inbox";
import ThreadReader from "./views/Thread";
import Composer from "./views/Composer";
import SearchOverlay from "./views/Search";
import ChordOverlay from "./views/ChordOverlay";

// Threads fetched per label-view load. We render the whole list (no
// windowing) so this should stay modest — bumping to 1000 covers Brian's
// 940-thread inbox while keeping initial render time in the low double-
// digit ms. v2 lands proper list virtualization for arbitrary inboxes.
const INBOX_LIMIT = 1000;

type AppView =
  | { kind: "inbox"; label: string; threads: ThreadSummary[]; selected: number }
  | { kind: "thread"; thread: ThreadSummary; messages: Message[]; selectedMsg: number }
  | { kind: "composer"; background: AppView }
  | {
      kind: "search";
      query: string;
      results: ThreadSummary[];
      selected: number;
      background: AppView;
    };

export default function App() {
  const [view, setView] = createSignal<AppView>({
    kind: "inbox",
    label: "INBOX",
    threads: [],
    selected: 0,
  });
  const [keymap, setKeymap] = createSignal<Keymap | null>(null);
  const [bootError, setBootError] = createSignal<string | null>(null);
  const [chordBuf, setChordBuf] = createSignal("");
  const [chordConts, setChordConts] = createSignal<
    { next: string; action_name: string }[]
  >([]);
  const [status] = createResource(fetchAccountStatus);

  // Boot: load keymap, load inbox.
  onMount(async () => {
    try {
      const sources = await fetchKeymapSources();
      try {
        const resolved = Keymap.fromToml(sources.defaults);
        if (sources.user) {
          resolved.merge(Keymap.fromToml(sources.user));
        }
        setKeymap(resolved);
      } catch (e) {
        console.error("[mach] keymap parse failed:", e);
        setBootError(`keymap parse: ${(e as Error).message ?? e}`);
        // continue — empty keymap still lets the user *see* the inbox
      }
      const threads = await listThreads("INBOX", INBOX_LIMIT);
      console.log(`[mach] loaded ${threads.length} threads`);
      setView({ kind: "inbox", label: "INBOX", threads, selected: 0 });
    } catch (e) {
      const msg = `boot failed: ${(e as Error).message ?? e}`;
      console.error("[mach]", e);
      setBootError(msg);
    }
  });

  // Key handler: route via keymap.
  function handleKey(e: KeyboardEvent) {
    const km = keymap();
    if (!km) {
      console.warn("[mach] keymap not loaded yet; key dropped", e.key);
      return;
    }
    const v = view();

    // Don't intercept keys destined for native text inputs. The composer
    // and search overlay have inputs/textareas the user is typing into;
    // only specific control chords (Esc, ⌘+Enter, ⌘+S) reach the keymap.
    const target = e.target as HTMLElement | null;
    const inTextInput =
      target &&
      (target.tagName === "INPUT" ||
        target.tagName === "TEXTAREA" ||
        target.isContentEditable);
    if (inTextInput && !isControlKey(e)) return;
    if (v.kind === "composer" || v.kind === "search") {
      if (!isControlKey(e)) return;
    }

    const chord = keyEventToChord(e);
    if (!chord) return;
    const newBuf = chordBuf() ? `${chordBuf()} ${chord}` : chord;
    const ctx = currentContext(v);
    const mode = currentMode(v);
    const r = km.resolve(mode, newBuf, ctx);
    console.debug("[mach] key", { chord: newBuf, mode, resolution: r.kind });
    if (r.kind === "action") {
      e.preventDefault();
      setChordBuf("");
      setChordConts([]);
      runAction(r.action);
    } else if (r.kind === "prefix") {
      e.preventDefault();
      setChordBuf(newBuf);
      setChordConts(r.continuations);
    } else {
      if (chordBuf()) {
        setChordBuf("");
        setChordConts([]);
        const fresh = km.resolve(mode, chord, ctx);
        if (fresh.kind === "action") {
          e.preventDefault();
          runAction(fresh.action);
        }
      }
    }
  }

  function currentContext(v: AppView): KeyContext {
    if (v.kind === "inbox") {
      const t = v.threads[v.selected];
      return {
        selection: t ? [t.id] : [],
        current_thread: t?.id,
      };
    }
    if (v.kind === "thread") {
      const m = v.messages[v.selectedMsg];
      return {
        selection: [v.thread.id],
        current_thread: v.thread.id,
        current_message: m?.id,
      };
    }
    if (v.kind === "search") {
      const t = v.results[v.selected];
      return {
        selection: t ? [t.id] : [],
        current_thread: t?.id,
      };
    }
    return { selection: [] };
  }

  function currentMode(v: AppView): Mode {
    switch (v.kind) {
      case "inbox":
        return "normal";
      case "thread":
        return "reading";
      case "composer":
        return "composing";
      case "search":
        return "search";
    }
  }

  async function runAction(action: Record<string, unknown>) {
    const kind = action.kind as string;
    const v = view();
    switch (kind) {
      case "quit":
        // window.close — but keep state for now; user can ⌘W or cmd+Q
        return;
      case "back_to_list": {
        const threads = await listThreads("INBOX", INBOX_LIMIT);
        setView({ kind: "inbox", label: "INBOX", threads, selected: 0 });
        return;
      }
      case "select_next":
        moveSelection(1);
        return;
      case "select_prev":
        moveSelection(-1);
        return;
      case "compose_new":
        setView({ kind: "composer", background: v });
        return;
      case "open_thread": {
        const id = (action.id as string) ?? "";
        const t = await openThreadIpc(id);
        setView({
          kind: "thread",
          thread: t.thread,
          messages: t.messages,
          selectedMsg: 0,
        });
        return;
      }
      case "open_label": {
        const label = action.label_id as string;
        const threads = await listThreads(label, INBOX_LIMIT);
        setView({ kind: "inbox", label, threads, selected: 0 });
        return;
      }
      case "search": {
        setView({
          kind: "search",
          query: "",
          results: [],
          selected: 0,
          background: v,
        });
        return;
      }
      case "refresh": {
        // Context-aware: in the inbox, re-pull the thread list. In a
        // thread, force-refetch the bodies (clears the cache and goes
        // back to Gmail for `format=full`).
        if (v.kind === "thread") {
          const opened = await refetchThreadIpc(v.thread.id);
          setView({
            kind: "thread",
            thread: opened.thread,
            messages: opened.messages,
            selectedMsg: v.selectedMsg,
          });
        } else if (v.kind === "inbox") {
          const threads = await listThreads(v.label, INBOX_LIMIT);
          setView({ ...v, threads });
        }
        return;
      }
    }
    // Pass-through to backend dispatcher for the rest (mutations).
    try {
      const outcome = await dispatchAction(action);
      const removedSet = new Set(outcome.changed_threads);
      const isArchiveOrTrash = kind === "archive" || kind === "trash";

      // From inbox: drop the row in place so the gap visibly closes.
      if (isArchiveOrTrash && v.kind === "inbox" && removedSet.size) {
        const newThreads = v.threads.filter((t) => !removedSet.has(t.id));
        setView({
          ...v,
          threads: newThreads,
          selected: Math.min(v.selected, Math.max(0, newThreads.length - 1)),
        });
      }

      // From thread view: bounce back to inbox and reload. The user just
      // archived/trashed this thread — they're done with it.
      if (isArchiveOrTrash && v.kind === "thread") {
        const threads = await listThreads("INBOX", INBOX_LIMIT);
        // Try to land selection where this thread *would have been* so the
        // next press of `j` moves naturally; falls back to top of list.
        const at = threads.findIndex((t) => t.id === v.thread.id);
        const fallback =
          at >= 0 ? Math.min(at, threads.length - 1) : 0;
        setView({
          kind: "inbox",
          label: "INBOX",
          threads,
          selected: fallback,
        });
      }
    } catch (e) {
      console.warn("dispatch failed", e);
    }
  }

  function moveSelection(delta: number) {
    const v = view();
    if (v.kind === "inbox") {
      const next = clamp(v.selected + delta, 0, v.threads.length - 1);
      setView({ ...v, selected: next });
    } else if (v.kind === "thread") {
      const next = clamp(v.selectedMsg + delta, 0, v.messages.length - 1);
      setView({ ...v, selectedMsg: next });
    } else if (v.kind === "search") {
      const next = clamp(v.selected + delta, 0, v.results.length - 1);
      setView({ ...v, selected: next });
    }
  }

  // Search-overlay text input + selection.
  async function onSearchInput(query: string) {
    const v = view();
    if (v.kind !== "search") return;
    const trimmed = query.trim();
    const results = trimmed ? await searchThreads(trimmed, 50) : [];
    setView({ ...v, query, results, selected: 0 });
  }

  async function onSearchEnter() {
    const v = view();
    if (v.kind !== "search") return;
    const t = v.results[v.selected];
    if (!t) return;
    const opened = await openThreadIpc(t.id);
    setView({
      kind: "thread",
      thread: opened.thread,
      messages: opened.messages,
      selectedMsg: 0,
    });
  }

  function onSearchEsc() {
    const v = view();
    if (v.kind !== "search") return;
    setView(v.background);
  }

  onMount(() => {
    // Listen on `document` (not just `window`) so events fire even when
    // focus is in the macOS overlay title bar region — Tauri's
    // `titleBarStyle: "Overlay"` sometimes routes keyboard there first.
    document.addEventListener("keydown", handleKey, true);
    // Make sure the body has focus so the very first key press registers
    // without requiring the user to click first.
    window.focus();
    document.body.tabIndex = -1;
    document.body.focus();
  });
  onCleanup(() => {
    document.removeEventListener("keydown", handleKey, true);
  });

  return (
    <div class="h-full w-full flex flex-col">
      <Show when={bootError()}>
        <div class="bg-danger/20 text-danger px-4 py-2 text-sm border-b border-danger/30">
          ⚠ {bootError()}
        </div>
      </Show>
      <TopBar accountEmail={status()?.email} online={status()?.online ?? false} view={view()} />
      <div class="flex-1 min-h-0 relative overflow-hidden">
        <Show when={view().kind === "inbox"}>
          <Inbox
            v={view() as Extract<AppView, { kind: "inbox" }>}
            onSelect={(i) => setView({ ...(view() as any), selected: i })}
            onOpen={async (i) => {
              const v = view() as Extract<AppView, { kind: "inbox" }>;
              const t = v.threads[i];
              if (!t) return;
              const opened = await openThreadIpc(t.id);
              setView({
                kind: "thread",
                thread: opened.thread,
                messages: opened.messages,
                selectedMsg: 0,
              });
            }}
          />
        </Show>
        <Show when={view().kind === "thread"}>
          <ThreadReader v={view() as Extract<AppView, { kind: "thread" }>} />
        </Show>
        <Show when={view().kind === "composer"}>
          <Composer
            onClose={() => {
              const current = view();
              if (current.kind === "composer") setView(current.background);
            }}
          />
        </Show>
        <Show when={view().kind === "search"}>
          <SearchOverlay
            v={view() as Extract<AppView, { kind: "search" }>}
            onInput={onSearchInput}
            onEnter={onSearchEnter}
            onEsc={onSearchEsc}
            onMove={(d) => moveSelection(d)}
          />
        </Show>
      </div>
      <StatusBar
        accountEmail={status()?.email}
        online={status()?.online ?? false}
        chordBuf={chordBuf()}
        chordConts={chordConts()}
      />
      <ChordOverlay show={chordBuf().length > 0} buf={chordBuf()} conts={chordConts()} />
    </div>
  );
}

function TopBar(props: { accountEmail?: string; online: boolean; view: AppView }) {
  const heading = () => {
    const v = props.view;
    if (v.kind === "inbox") return labelDisplay(v.label);
    if (v.kind === "thread") return v.thread.subject || "(no subject)";
    if (v.kind === "composer") return "New message";
    if (v.kind === "search") return "Search";
    return "";
  };
  const subheading = () => {
    const v = props.view;
    if (v.kind === "inbox") return `${v.threads.length.toLocaleString()} threads`;
    if (v.kind === "thread")
      return `${v.thread.participants.slice(0, 2).join(", ")}${
        v.thread.participants.length > 2 ? ` +${v.thread.participants.length - 2}` : ""
      }`;
    return props.accountEmail ?? "";
  };
  return (
    <div
      data-tauri-drag-region
      class="flex items-center gap-3 px-5 select-none border-b border-fg-dim/10"
      style={{ height: "52px" }}
    >
      {/* Spacer for the macOS traffic lights — overlay title bar mode puts
          them on top of our content, so leave a gap on the left. */}
      <div
        data-tauri-drag-region
        style={{ width: "68px", flex: "0 0 68px" }}
        aria-hidden
      />
      <div data-tauri-drag-region class="flex-1 min-w-0">
        <div
          data-tauri-drag-region
          class="text-fg text-[13px] font-semibold tracking-tight truncate"
        >
          {heading()}
        </div>
        <div
          data-tauri-drag-region
          class="text-fg-mute text-[11px] truncate"
        >
          {subheading()}
        </div>
      </div>
      <div class="flex items-center gap-2 shrink-0">
        <span class="text-fg-mute text-[11px]">{props.accountEmail}</span>
        <span
          class={
            "flex items-center gap-1.5 text-[11px] font-medium px-2 py-0.5 rounded-full " +
            (props.online ? "text-success" : "text-fg-mute")
          }
          style={{
            background: props.online
              ? "color-mix(in oklab, var(--color-success) 14%, transparent)"
              : "color-mix(in oklab, var(--color-fg-dim) 14%, transparent)",
          }}
        >
          <span
            class="w-1.5 h-1.5 rounded-full"
            style={{
              background: props.online ? "var(--color-success)" : "var(--color-fg-mute)",
              "box-shadow": props.online
                ? "0 0 6px color-mix(in oklab, var(--color-success) 60%, transparent)"
                : "none",
            }}
          />
          {props.online ? "Live" : "Offline"}
        </span>
      </div>
    </div>
  );
}

function StatusBar(props: {
  accountEmail?: string;
  online: boolean;
  chordBuf: string;
  chordConts: { next: string; action_name: string }[];
}) {
  return (
    <div class="px-5 py-2 flex items-center text-[11px] text-fg-mute border-t border-fg-dim/10 select-none">
      <Show
        when={props.chordBuf}
        fallback={
          <div class="flex items-center gap-4">
            <Hint k="j/k" label="nav" />
            <Hint k="e" label="archive" />
            <Hint k="c" label="compose" />
            <Hint k="/" label="search" />
            <Hint k="r" label="reply" />
            <Hint k="esc" label="back" />
          </div>
        }
      >
        <span class="text-accent font-mono mr-2">{props.chordBuf}</span>
        <span class="text-fg-mute mr-2">→</span>
        <div class="flex items-center gap-3">
          {props.chordConts.map((c) => (
            <span class="flex items-center gap-1">
              <span class="kbd">{c.next}</span>
              <span>{c.action_name.replace(/_/g, " ")}</span>
            </span>
          ))}
        </div>
      </Show>
    </div>
  );
}

function Hint(props: { k: string; label: string }) {
  return (
    <span class="flex items-center gap-1.5">
      <span class="kbd">{props.k}</span>
      <span class="text-fg-mute">{props.label}</span>
    </span>
  );
}

function clamp(v: number, lo: number, hi: number) {
  if (hi < lo) return lo;
  return Math.min(Math.max(v, lo), hi);
}

function isControlKey(e: KeyboardEvent) {
  if (e.key === "Escape") return true;
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) return true;
  if (e.key === "s" && (e.ctrlKey || e.metaKey)) return true;
  return false;
}

function truncate(s: string, n: number) {
  return s.length <= n ? s : s.slice(0, n - 1) + "…";
}

function labelDisplay(id: string) {
  const map: Record<string, string> = {
    INBOX: "Inbox",
    STARRED: "Starred",
    SENT: "Sent",
    DRAFT: "Drafts",
    TRASH: "Trash",
    SPAM: "Spam",
    DONE: "Done",
  };
  return map[id] ?? id;
}

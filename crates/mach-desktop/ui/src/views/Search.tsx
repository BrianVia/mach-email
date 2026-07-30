// Search overlay. Big input at top, results list below, debounced filter.
import { For, onMount, createMemo } from "solid-js";
import type { ThreadSummary } from "../lib/ipc";
import { avatarColor, initialsFor } from "../lib/avatar";
import { Search as SearchIcon } from "../lib/Icon";

export default function SearchOverlay(props: {
  v: {
    kind: "search";
    query: string;
    results: ThreadSummary[];
    selected: number;
    background: unknown;
  };
  onInput: (q: string) => void;
  onEnter: () => void;
  onEsc: () => void;
  onMove: (d: number) => void;
}) {
  let input: HTMLInputElement | undefined;
  onMount(() => input?.focus());

  let timer: number | undefined;
  function debouncedInput(q: string) {
    if (timer) window.clearTimeout(timer);
    timer = window.setTimeout(() => props.onInput(q), 80);
  }

  return (
    <div
      class="absolute inset-0 z-20 flex flex-col"
      style={{
        background: "color-mix(in oklab, var(--color-bg) 80%, transparent)",
        "backdrop-filter": "blur(12px)",
      }}
    >
      <div class="border-b border-fg-dim/10 px-6 py-5 flex items-center gap-3">
        <SearchIcon size={18} class="text-fg-mute" />
        <input
          ref={input}
          class="flex-1 bg-transparent outline-none text-fg text-[15px] placeholder:text-fg-mute"
          placeholder="Search subjects, senders, body…"
          value={props.v.query}
          onInput={(e) => debouncedInput(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              props.onEsc();
            } else if (e.key === "Enter") {
              e.preventDefault();
              props.onEnter();
            } else if (e.key === "ArrowDown") {
              e.preventDefault();
              props.onMove(1);
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              props.onMove(-1);
            }
          }}
        />
        <span class="text-[11.5px] text-fg-mute tabular-nums">
          {props.v.results.length || 0}
        </span>
      </div>
      <div class="flex-1 overflow-y-auto">
        <For each={props.v.results}>
          {(t, i) => <SearchRow t={t} selected={i() === props.v.selected} />}
        </For>
      </div>
    </div>
  );
}

function SearchRow(props: { t: ThreadSummary; selected: boolean }) {
  const senderRaw = createMemo(() => props.t.participants[0] ?? "(no sender)");
  const initials = createMemo(() => initialsFor(senderRaw()));
  const avatarBg = createMemo(() => avatarColor(senderRaw()));
  return (
    <div
      class={`flex items-center gap-3 px-6 py-3 cursor-pointer transition-colors ${
        props.selected ? "bg-accent/15" : "hover:bg-bg-elev-3/40"
      }`}
    >
      <span class="avatar" style={{ background: avatarBg() }} aria-hidden>
        {initials()}
      </span>
      <div class="flex-1 min-w-0">
        <p class="text-fg-mute text-[10px] truncate">
          {props.t.account_id}
        </p>
        <p class="text-fg text-[13.5px] font-medium truncate">
          {props.t.subject}
        </p>
        <p class="text-fg-mute text-[12px] truncate">
          {senderRaw()} — {props.t.snippet}
        </p>
      </div>
    </div>
  );
}

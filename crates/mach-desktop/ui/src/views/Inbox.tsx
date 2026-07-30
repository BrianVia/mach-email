// Inbox list with refined typography + sender avatars. Single-row layout
// with hierarchy: avatar · sender · subject + snippet · time.
import { createMemo, For, Show, createEffect } from "solid-js";
import type { ThreadSummary } from "../lib/ipc";
import { avatarColor, initialsFor } from "../lib/avatar";
import { Star } from "../lib/Icon";

const ROW_H = 64;

export default function Inbox(props: {
  v: {
    kind: "inbox";
    label: string;
    threads: ThreadSummary[];
    selected: number;
  };
  onSelect: (i: number) => void;
  onOpen: (i: number) => void;
}) {
  let highlight: HTMLDivElement | undefined;
  let scroller: HTMLDivElement | undefined;

  // Selection moves via CSS transform — GPU-composited, no animation queue
  // when holding j/k. Also auto-scroll the selected row into view.
  createEffect(() => {
    const i = props.v.selected;
    if (!highlight) return;
    highlight.style.transform = `translateY(${i * ROW_H}px)`;

    if (!scroller) return;
    const top = i * ROW_H;
    const bottom = top + ROW_H;
    const viewTop = scroller.scrollTop;
    const viewBottom = viewTop + scroller.clientHeight;
    if (top < viewTop) scroller.scrollTo({ top, behavior: "instant" as ScrollBehavior });
    else if (bottom > viewBottom)
      scroller.scrollTo({ top: bottom - scroller.clientHeight, behavior: "instant" as ScrollBehavior });
  });

  const list = createMemo(() => props.v.threads);

  return (
    <div
      ref={scroller}
      class="relative h-full overflow-y-auto"
      role="list"
    >
      <Show when={list().length === 0}>
        <div class="flex flex-col items-center justify-center h-full text-fg-mute select-none gap-2">
          <span class="text-xl">Inbox zero</span>
          <span class="text-sm">Nothing in this view.</span>
        </div>
      </Show>
      <div
        ref={highlight}
        class="absolute left-0 right-0 row-selected pointer-events-none"
        style={{
          height: `${ROW_H}px`,
          top: 0,
          transition: "transform 90ms cubic-bezier(0.22, 1, 0.36, 1)",
          "will-change": "transform",
        }}
      />
      <For each={list()}>
        {(t, i) => (
          <Row
            t={t}
            selected={i() === props.v.selected}
            onClick={() => props.onSelect(i())}
            onDoubleClick={() => props.onOpen(i())}
          />
        )}
      </For>
    </div>
  );
}

function Row(props: {
  t: ThreadSummary;
  selected: boolean;
  onClick: () => void;
  onDoubleClick: () => void;
}) {
  const senderRaw = createMemo(() => props.t.participants[0] ?? "(no sender)");
  const senderName = createMemo(() => prettySender(senderRaw()));
  const initials = createMemo(() => initialsFor(senderRaw()));
  const avatarBg = createMemo(() => avatarColor(senderRaw()));
  const when = createMemo(() => prettyDate(props.t.last_message_at));

  return (
    <div
      class={`
        relative flex items-center gap-3 px-5 cursor-pointer select-none
        transition-colors duration-150
        ${props.selected ? "" : "hover:bg-bg-elev-3/40"}
      `}
      style={{ height: `${ROW_H}px` }}
      onClick={props.onClick}
      onDblClick={props.onDoubleClick}
      role="listitem"
    >
      {/* Unread + star indicator column */}
      <div class="flex flex-col items-center gap-1 w-3 shrink-0">
        <Show
          when={props.t.unread}
          fallback={<span class="w-2 h-2" aria-hidden />}
        >
          <span class="unread-dot w-2 h-2" aria-label="unread" />
        </Show>
        <Show when={props.t.starred}>
          <Star size={10} class="text-starred" aria-label="starred" />
        </Show>
      </div>

      {/* Avatar */}
      <span
        class="avatar"
        style={{ background: avatarBg() }}
        aria-hidden
      >
        {initials()}
      </span>

      {/* Sender + subject + snippet */}
      <div class="flex-1 min-w-0">
        <div class="flex items-baseline gap-2">
          <span
            class={`truncate text-sm ${
              props.t.unread ? "text-fg font-semibold" : "text-fg-soft font-medium"
            }`}
            style={{ "max-width": "30%" }}
          >
            {senderName()}
          </span>
          <span class={`truncate ${props.t.unread ? "text-fg" : "text-fg-soft"}`}>
            <span class={props.t.unread ? "font-semibold" : "font-normal"}>
              {props.t.subject || "(no subject)"}
            </span>
            <span class="text-fg-mute"> — {props.t.snippet}</span>
          </span>
        </div>
      </div>

      {/* Date */}
      <div class={`shrink-0 text-xs tabular-nums ${props.t.unread ? "text-fg-soft" : "text-fg-mute"}`}>
        {when()}
      </div>
    </div>
  );
}

/**
 * Strip a quoted "Display Name" out of a `Name <email>` and bias toward
 * the email's @domain when no name is present.
 */
function prettySender(raw: string): string {
  const m = raw.match(/^\s*"?([^"<]+?)"?\s*<.+>$/);
  if (m) return m[1].trim();
  const e = raw.match(/^([\w.+-]+)@([\w.-]+)$/);
  if (e) return e[2]; // domain — feels more recognizable than the local part
  return raw;
}

function prettyDate(iso: string): string {
  const d = new Date(iso);
  const now = Date.now();
  const diff = now - d.getTime();
  const min = 60 * 1000;
  const hr = 60 * min;
  const day = 24 * hr;
  if (diff < day) return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  if (diff < 7 * day) return d.toLocaleDateString([], { weekday: "short" });
  if (diff < 365 * day) return d.toLocaleDateString([], { month: "short", day: "numeric" });
  return d.toLocaleDateString([], { year: "numeric" });
}

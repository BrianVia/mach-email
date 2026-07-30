// Thread reader. Subject as a hero, then a stack of message cards with
// avatar + sender + collapsible body. Selected message expands; others
// collapse to a 2-line preview the user can click open.
import { createSignal, For, createMemo, Show } from "solid-js";
import type { Message, ThreadSummary } from "../lib/ipc";
import { linkify } from "../lib/text";
import { renderEmailHtml } from "../lib/html";
import { avatarColor, initialsFor } from "../lib/avatar";

export default function ThreadReader(props: {
  v: {
    kind: "thread";
    thread: ThreadSummary;
    messages: Message[];
    selectedMsg: number;
  };
}) {
  return (
    <div class="h-full overflow-y-auto">
      <div class="max-w-3xl mx-auto px-8 py-8">
        <header class="mb-8">
          <h1 class="text-[22px] font-semibold tracking-tight text-fg leading-tight">
            {props.v.thread.subject || "(no subject)"}
          </h1>
          <p class="mt-2 text-[12px] text-fg-mute uppercase tracking-wider">
            {props.v.messages.length} {props.v.messages.length === 1 ? "message" : "messages"} ·{" "}
            {props.v.thread.participants.length}{" "}
            {props.v.thread.participants.length === 1 ? "participant" : "participants"}
          </p>
        </header>

        <div class="flex flex-col gap-3">
          <For each={props.v.messages}>
            {(m, i) => <MessageCard m={m} selected={i() === props.v.selectedMsg} />}
          </For>
        </div>
      </div>
    </div>
  );
}

function MessageCard(props: { m: Message; selected: boolean }) {
  const [expanded, setExpanded] = createSignal(props.selected);
  const dateStr = createMemo(() => prettyFullDate(props.m.internal_date));
  const initials = createMemo(() => initialsFor(props.m.from));
  const avatarBg = createMemo(() => avatarColor(props.m.from));
  const senderName = createMemo(() => {
    const m = props.m.from.match(/^\s*"?([^"<]+?)"?\s*<.+>$/);
    return m ? m[1].trim() : props.m.from;
  });
  const senderEmail = createMemo(() => {
    const m = props.m.from.match(/<([^>]+)>/);
    return m ? m[1] : "";
  });

  return (
    <article
      class={`
        rounded-[12px] overflow-hidden transition-colors
        ${props.selected ? "bg-bg-elev-2" : "bg-bg-elev-1 hover:bg-bg-elev-2"}
      `}
      style={{
        "box-shadow": props.selected ? "var(--shadow-card)" : "none",
        border: props.selected
          ? "1px solid color-mix(in oklab, var(--color-accent) 30%, transparent)"
          : "1px solid transparent",
      }}
    >
      <header
        class="flex items-start gap-3 px-5 py-4 cursor-pointer select-none"
        onClick={() => setExpanded((x) => !x)}
      >
        <span class="avatar" style={{ background: avatarBg() }} aria-hidden>
          {initials()}
        </span>
        <div class="flex-1 min-w-0">
          <div class="flex items-baseline gap-2 flex-wrap">
            <span class="font-semibold text-fg text-[14px] truncate">
              {senderName()}
            </span>
            <Show when={senderEmail()}>
              <span class="text-fg-mute text-[12px] truncate">{senderEmail()}</span>
            </Show>
          </div>
          <p class="text-fg-mute text-[11.5px] mt-0.5">
            to {props.m.to.join(", ") || "—"}
          </p>
          <Show when={!expanded()}>
            <p class="text-fg-mute text-[13px] mt-2 line-clamp-2">
              {(props.m.body_plain ?? props.m.snippet ?? "").slice(0, 200)}
            </p>
          </Show>
        </div>
        <span class="text-[11px] text-fg-mute shrink-0 tabular-nums">
          {dateStr()}
        </span>
      </header>

      <Show when={expanded()}>
        <Show
          when={props.m.body_html && props.m.body_html.length > 0}
          fallback={
            <div
              class="px-5 pb-5 text-[14px] whitespace-pre-wrap break-words text-fg-soft leading-relaxed"
              innerHTML={linkify(
                props.m.body_plain ?? props.m.snippet ?? "(no body)",
              )}
            />
          }
        >
          <div
            class="px-5 pb-5 text-[14px] break-words text-fg-soft leading-relaxed mach-html"
            innerHTML={renderEmailHtml(props.m).html}
          />
        </Show>
      </Show>
    </article>
  );
}

function prettyFullDate(iso: string): string {
  const d = new Date(iso);
  const now = Date.now();
  const diff = now - d.getTime();
  const day = 24 * 60 * 60 * 1000;
  if (diff < day) {
    return d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  }
  return d.toLocaleDateString([], {
    weekday: "short",
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

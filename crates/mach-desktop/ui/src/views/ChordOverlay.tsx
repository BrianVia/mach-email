// Chord prefix overlay — shows the available continuations after a chord
// key is held (g → i:inbox, s:starred, …). Slides up from bottom-right.
import { Show, For } from "solid-js";

export default function ChordOverlay(props: {
  show: boolean;
  buf: string;
  conts: { next: string; action_name: string }[];
}) {
  return (
    <Show when={props.show}>
      <div
        class="absolute bottom-12 right-6 rounded-[12px] p-4 min-w-[280px] z-40 glass"
        style={{ "box-shadow": "var(--shadow-pop)" }}
      >
        <div class="text-[11px] uppercase tracking-wider text-fg-mute mb-3">
          Prefix <span class="kbd">{props.buf}</span>
        </div>
        <ul class="flex flex-col gap-1.5">
          <For each={props.conts}>
            {(c) => (
              <li class="flex items-center gap-3 text-[13px]">
                <span class="kbd">{c.next}</span>
                <span class="text-fg-soft">{c.action_name.replace(/_/g, " ")}</span>
              </li>
            )}
          </For>
        </ul>
      </div>
    </Show>
  );
}

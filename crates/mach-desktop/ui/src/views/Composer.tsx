// Compose modal — backdrop blur, generous spacing, no field borders
// except a subtle bottom rule that focuses the eye on the body editor.
import { createSignal, onMount } from "solid-js";
import { Send } from "../lib/Icon";

export default function Composer(props: { onClose: () => void }) {
  const [to, setTo] = createSignal("");
  const [cc, setCc] = createSignal("");
  const [subject, setSubject] = createSignal("");
  const [body, setBody] = createSignal("");
  let firstInput: HTMLInputElement | undefined;
  onMount(() => firstInput?.focus());

  return (
    <div
      class="absolute inset-0 z-30 flex items-start justify-center pt-24 px-4"
      style={{
        background: "color-mix(in oklab, var(--color-bg) 60%, transparent)",
        "backdrop-filter": "blur(8px)",
      }}
      onClick={(e) => {
        if (e.currentTarget === e.target) props.onClose();
      }}
    >
      <div
        class="rounded-[16px] w-[680px] max-w-[92vw] overflow-hidden glass"
        style={{ "box-shadow": "var(--shadow-pop)" }}
      >
        <header class="px-6 pt-5 pb-4 flex items-center justify-between border-b border-fg-dim/15">
          <h2 class="text-[15px] font-semibold tracking-tight">New message</h2>
          <button
            class="text-fg-mute hover:text-fg text-[12px]"
            onClick={props.onClose}
          >
            esc
          </button>
        </header>

        <div class="px-6">
          <Field label="To" value={to()} onInput={setTo} ref_={(r) => (firstInput = r)} />
          <Field label="Cc" value={cc()} onInput={setCc} />
          <Field label="Subject" value={subject()} onInput={setSubject} />
        </div>

        <textarea
          class="w-full bg-transparent px-6 py-4 text-[14px] leading-relaxed h-56 outline-none resize-none placeholder:text-fg-mute"
          placeholder="Write your message…"
          value={body()}
          onInput={(e) => setBody(e.currentTarget.value)}
        />

        <footer class="px-6 py-3 border-t border-fg-dim/15 flex items-center justify-between text-[11.5px] text-fg-mute">
          <div class="flex items-center gap-3">
            <span class="kbd">⌘↵</span> send · <span class="kbd">⌘S</span> save · <span class="kbd">esc</span> close
          </div>
          <button
            class="flex items-center gap-2 px-3 py-1.5 rounded-full bg-accent/15 text-accent hover:bg-accent/25 transition-colors text-[12.5px] font-medium"
            onClick={() => {
              /* placeholder — send wiring lives in the outbox + send-later loop */
            }}
            disabled
            title="send wiring lands with sync engine (see tasks/todo.md)"
          >
            <Send size={13} />
            Send
          </button>
        </footer>
      </div>
    </div>
  );
}

function Field(props: {
  label: string;
  value: string;
  onInput: (s: string) => void;
  ref_?: (el: HTMLInputElement) => void;
}) {
  return (
    <div class="flex items-center gap-4 border-b border-fg-dim/10 py-3">
      <label class="w-16 text-fg-mute text-[12px] uppercase tracking-wider">
        {props.label}
      </label>
      <input
        ref={props.ref_}
        class="flex-1 bg-transparent text-[14px] outline-none placeholder:text-fg-mute"
        value={props.value}
        onInput={(e) => props.onInput(e.currentTarget.value)}
      />
    </div>
  );
}

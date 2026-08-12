<script lang="ts">
  import { onMount, untrack } from "svelte";
  import Icon from "../lib/Icon.svelte";
  import type { Draft } from "../lib/ipc";

  export type ComposerFields = { to: string; cc: string; subject: string; body_md: string };

  let {
    draft,
    onFieldsChange,
    onSend,
    onClose,
  }: {
    draft: Draft;
    onFieldsChange: (fields: ComposerFields) => void;
    onSend: () => void;
    onClose: () => void;
  } = $props();
  let to = $state(untrack(() => draft.to.join(", ")));
  let cc = $state(untrack(() => draft.cc.join(", ")));
  let subject = $state(untrack(() => draft.subject));
  let body = $state(untrack(() => draft.body_md));
  let firstInput: HTMLInputElement;

  onMount(() => firstInput?.focus());

  function fieldsChanged() {
    onFieldsChange({ to, cc, subject, body_md: body });
  }
</script>

<div class="backdrop" role="presentation" onclick={(event) => event.currentTarget === event.target && onClose()}>
  <div class="modal" role="dialog" aria-modal="true" aria-labelledby="composer-title">
    <header>
      <h2 id="composer-title">New message</h2>
      <button class="escape" onclick={onClose}>esc</button>
    </header>
    <div class="fields">
      <label><span>To</span><input bind:this={firstInput} bind:value={to} oninput={fieldsChanged} /></label>
      <label><span>Cc</span><input bind:value={cc} oninput={fieldsChanged} /></label>
      <label><span>Subject</span><input bind:value={subject} oninput={fieldsChanged} /></label>
    </div>
    <textarea bind:value={body} oninput={fieldsChanged} placeholder="Write your message…"></textarea>
    <footer>
      <div><kbd>⌘↵</kbd> send · <kbd>⌘S</kbd> save · <kbd>esc</kbd> close</div>
      <button class="send" onclick={onSend}>
        <Icon name="send" size={13} /> Send
      </button>
    </footer>
  </div>
</div>

<style>
  .backdrop { position: absolute; inset: 0; z-index: 30; display: flex; align-items: flex-start; justify-content: center; padding: 96px 16px 16px; background: color-mix(in oklab, var(--bg) 60%, transparent); backdrop-filter: blur(8px); }
  .modal { width: 680px; max-width: 92vw; overflow: hidden; border-radius: 16px; background: var(--surface); box-shadow: 0 20px 64px color-mix(in oklab, black 35%, transparent); }
  header { display: flex; align-items: center; justify-content: space-between; padding: 20px 24px 16px; border-bottom: 1px solid var(--border); }
  h2 { margin: 0; font-size: 15px; font-weight: 600; letter-spacing: -.01em; }
  .escape { border: 0; background: transparent; color: var(--muted); font-size: 12px; }
  .escape:hover { color: var(--text); }
  .fields { padding: 0 24px; }
  label { display: flex; align-items: center; gap: 16px; padding: 12px 0; border-bottom: 1px solid var(--border); }
  label span { width: 64px; color: var(--muted); font-size: 12px; letter-spacing: .08em; text-transform: uppercase; }
  input, textarea { border: 0; background: transparent; color: var(--text); font-family: inherit; outline: 0; }
  input { min-width: 0; flex: 1; font-size: 14px; }
  textarea { width: 100%; height: 14rem; padding: 16px 24px; resize: none; font-size: 14px; line-height: 1.6; }
  textarea::placeholder { color: var(--muted); }
  footer { display: flex; align-items: center; justify-content: space-between; padding: 12px 24px; border-top: 1px solid var(--border); color: var(--muted); font-size: 11.5px; }
  kbd { padding: 2px 5px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-2); font-family: var(--font-mono); font-size: 11px; }
  .send { display: inline-flex; align-items: center; gap: 8px; padding: 6px 12px; border: 0; border-radius: 999px; background: color-mix(in oklab, var(--accent) 15%, transparent); color: var(--accent); font-size: 12.5px; font-weight: 500; }
</style>

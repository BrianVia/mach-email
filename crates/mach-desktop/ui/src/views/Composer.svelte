<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import Icon from "../lib/Icon.svelte";
  import type { Draft } from "../lib/ipc";
  import { detectSnippetToken, type SnippetToken } from "../lib/snippet";

  export type ComposerFields = { to: string; cc: string; bcc: string; subject: string; body_md: string };

  let {
    draft,
    title,
    onFieldsChange,
    onSend,
    presets,
    onSchedule,
    onClose,
    snippets,
  }: {
    draft: Draft;
    title: string;
    onFieldsChange: (fields: ComposerFields) => void;
    onSend: () => void;
    presets: [string, string][];
    onSchedule: (at: string) => void;
    onClose: () => void;
    snippets: Record<string, string>;
  } = $props();
  let to = $state(untrack(() => draft.to.join(", ")));
  let cc = $state(untrack(() => draft.cc.join(", ")));
  let bcc = $state(untrack(() => draft.bcc.join(", ")));
  let subject = $state(untrack(() => draft.subject));
  let body = $state(untrack(() => draft.body_md));
  let firstInput: HTMLInputElement;
  let bodyInput: HTMLTextAreaElement;
  let snippetToken = $state<SnippetToken | null>(null);
  let snippetSelected = $state(0);
  let filteredSnippets = $derived(
    Object.entries(snippets).filter(([name]) =>
      name.toLocaleLowerCase().includes(snippetToken?.query.toLocaleLowerCase() ?? ""),
    ),
  );

  onMount(() => firstInput?.focus());

  function fieldsChanged() {
    onFieldsChange({ to, cc, bcc, subject, body_md: body });
  }

  function bodyChanged(event: Event) {
    const input = event.currentTarget as HTMLTextAreaElement;
    body = input.value;
    snippetToken = detectSnippetToken(input.value, input.selectionStart);
    snippetSelected = 0;
    fieldsChanged();
  }

  async function insertSnippet(index = snippetSelected) {
    const token = snippetToken;
    const entry = filteredSnippets[index];
    if (!token || !entry) return;
    const cursor = bodyInput.selectionStart;
    body = body.slice(0, token.start) + entry[1] + body.slice(cursor);
    snippetToken = null;
    fieldsChanged();
    await tick();
    const nextCursor = token.start + entry[1].length;
    bodyInput.setSelectionRange(nextCursor, nextCursor);
    bodyInput.focus();
  }

  function pickTime(event: Event) {
    const value = (event.currentTarget as HTMLInputElement).value;
    if (value) onSchedule(new Date(value).toISOString());
  }
</script>

<div class="backdrop" role="presentation" onclick={(event) => event.currentTarget === event.target && onClose()}>
  <div class="modal" role="dialog" aria-modal="true" aria-labelledby="composer-title">
    <header>
      <h2 id="composer-title">{title}</h2>
      <button class="escape" onclick={onClose}>esc</button>
    </header>
    <div class="fields">
      <label><span>To</span><input bind:this={firstInput} bind:value={to} oninput={fieldsChanged} /></label>
      <label><span>Cc</span><input bind:value={cc} oninput={fieldsChanged} /></label>
      <label><span>Bcc</span><input bind:value={bcc} oninput={fieldsChanged} /></label>
      <label><span>Subject</span><input bind:value={subject} oninput={fieldsChanged} /></label>
    </div>
    <div class="body-field">
      <textarea
        bind:this={bodyInput}
        bind:value={body}
        oninput={bodyChanged}
        onkeydown={(event) => {
          if (!snippetToken) return;
          if (event.key === "Escape") { event.preventDefault(); snippetToken = null; }
          else if (event.key === "Enter") { event.preventDefault(); void insertSnippet(); }
          else if (event.key === "ArrowDown") { event.preventDefault(); snippetSelected = Math.min(snippetSelected + 1, filteredSnippets.length - 1); }
          else if (event.key === "ArrowUp") { event.preventDefault(); snippetSelected = Math.max(snippetSelected - 1, 0); }
        }}
        placeholder="Write your message…"
      ></textarea>
      {#if snippetToken}
        <div class="snippet-picker" role="listbox" aria-label="Snippets">
          {#each filteredSnippets as snippet, index (snippet[0])}
            <button
              type="button"
              class:selected={index === snippetSelected}
              role="option"
              aria-selected={index === snippetSelected}
              onclick={() => insertSnippet(index)}
            >
              <span>{snippet[0]}</span><span class="preview">{snippet[1]}</span>
            </button>
          {/each}
        </div>
      {/if}
    </div>
    <footer>
      <div><kbd>⌘↵</kbd> send · <kbd>⌘S</kbd> save · <kbd>esc</kbd> close</div>
      <div class="actions">
        <button class="send" onclick={onSend}><Icon name="send" size={13} /> Send</button>
        <details>
          <summary>Later ▾</summary>
          <div class="later-menu">
            {#each presets as [label, at]}
              <button type="button" onclick={() => onSchedule(at)}>{label}</button>
            {/each}
            <label class="pick">Pick time<input type="datetime-local" onchange={pickTime} /></label>
          </div>
        </details>
      </div>
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
  .body-field { position: relative; }
  textarea { display: block; width: 100%; height: 14rem; padding: 16px 24px; resize: none; font-size: 14px; line-height: 1.6; }
  textarea::placeholder { color: var(--muted); }
  .snippet-picker { position: absolute; right: 24px; bottom: 12px; left: 24px; max-height: 10rem; overflow-y: auto; padding: 4px 0; border: 1px solid var(--border); border-radius: 10px; background: var(--surface); box-shadow: 0 12px 36px color-mix(in oklab, black 35%, transparent); }
  .snippet-picker button { display: flex; width: 100%; justify-content: space-between; gap: 16px; padding: 9px 12px; border: 0; background: transparent; color: inherit; font: inherit; font-size: 13px; text-align: left; }
  .snippet-picker button:not(.selected):hover { background: var(--hover); }
  .snippet-picker button.selected { background: color-mix(in oklab, var(--accent) 15%, transparent); }
  .preview { overflow: hidden; color: var(--muted); text-overflow: ellipsis; white-space: nowrap; }
  footer { display: flex; align-items: center; justify-content: space-between; padding: 12px 24px; border-top: 1px solid var(--border); color: var(--muted); font-size: 11.5px; }
  kbd { padding: 2px 5px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-2); font-family: var(--font-mono); font-size: 11px; }
  .send { display: inline-flex; align-items: center; gap: 8px; padding: 6px 12px; border: 0; border-radius: 999px; background: color-mix(in oklab, var(--accent) 15%, transparent); color: var(--accent); font-size: 12.5px; font-weight: 500; }
  .actions { position: relative; display: flex; align-items: center; gap: 8px; }
  details summary { padding: 6px 10px; border-radius: 999px; color: var(--accent); cursor: pointer; list-style: none; }
  .later-menu { position: absolute; right: 0; bottom: 34px; z-index: 2; width: 190px; padding: 6px; border: 1px solid var(--border); border-radius: 10px; background: var(--surface); box-shadow: 0 10px 30px color-mix(in oklab, black 25%, transparent); }
  .later-menu button, .pick { display: block; box-sizing: border-box; width: 100%; padding: 8px; border: 0; border-radius: 6px; background: transparent; color: var(--text); font: inherit; text-align: left; }
  .later-menu button:hover, .pick:hover { background: var(--hover); }
  .pick input { display: block; width: 100%; margin-top: 6px; color-scheme: dark; }
</style>

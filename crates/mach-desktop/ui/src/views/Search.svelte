<script lang="ts">
  import { onMount } from "svelte";
  import type { ThreadSummary } from "../lib/ipc";
  import { avatarColor, initialsFor } from "../lib/avatar";
  import Icon from "../lib/Icon.svelte";

  let {
    v,
    onInput,
    onEnter,
    onEsc,
    onMove,
    onPick,
  }: {
    v: { kind: "search"; query: string; results: ThreadSummary[]; selected: number; background: unknown };
    onInput: (query: string) => void;
    onEnter: () => void;
    onEsc: () => void;
    onMove: (delta: number) => void;
    onPick: (index: number) => void;
  } = $props();

  let input: HTMLInputElement;
  let timer: number | undefined;
  onMount(() => {
    input?.focus();
    return () => timer !== undefined && window.clearTimeout(timer);
  });

  function debouncedInput(query: string) {
    if (timer !== undefined) window.clearTimeout(timer);
    timer = window.setTimeout(() => onInput(query), 80);
  }
</script>

<div class="overlay">
  <div class="searchbar">
    <Icon name="search" size={18} class="search-icon" />
    <input
      bind:this={input}
      value={v.query}
      placeholder="Search subjects, senders, body…"
      oninput={(event) => debouncedInput(event.currentTarget.value)}
      onkeydown={(event) => {
        if (event.key === "Escape") { event.preventDefault(); onEsc(); }
        else if (event.key === "Enter") { event.preventDefault(); onEnter(); }
        else if (event.key === "ArrowDown") { event.preventDefault(); onMove(1); }
        else if (event.key === "ArrowUp") { event.preventDefault(); onMove(-1); }
      }}
    />
    <span class="count">{v.results.length}</span>
  </div>
  <div class="results">
    {#each v.results as thread, index (thread.id)}
      {@const sender = thread.participants[0] ?? "(no sender)"}
      <button type="button" class:selected={index === v.selected} class="result" onclick={() => onPick(index)}>
        <span class="avatar" style:background={avatarColor(sender)} aria-hidden="true">{initialsFor(sender)}</span>
        <div>
          <p class="account">{thread.account_id}</p>
          <p class="subject">{thread.subject}</p>
          <p class="snippet">{sender} — {thread.snippet}</p>
        </div>
      </button>
    {/each}
  </div>
</div>

<style>
  .overlay { position: absolute; inset: 0; z-index: 20; display: flex; flex-direction: column; background: color-mix(in oklab, var(--bg) 80%, transparent); backdrop-filter: blur(12px); }
  .searchbar { display: flex; align-items: center; gap: 12px; padding: 20px 24px; border-bottom: 1px solid var(--border); }
  :global(.search-icon) { flex-shrink: 0; color: var(--muted); }
  input { min-width: 0; flex: 1; border: 0; background: transparent; color: var(--text); font-size: 15px; outline: 0; }
  input::placeholder { color: var(--muted); }
  .count { color: var(--muted); font-size: 11.5px; font-variant-numeric: tabular-nums; }
  .results { flex: 1; overflow-y: auto; }
  .result { display: flex; width: 100%; align-items: center; gap: 12px; padding: 12px 24px; border: 0; background: transparent; color: inherit; font: inherit; text-align: left; cursor: pointer; transition: background 150ms; }
  .result:not(.selected):hover { background: var(--hover); }
  .result.selected { background: color-mix(in oklab, var(--accent) 15%, transparent); }
  .avatar { display: inline-flex; width: 32px; height: 32px; flex-shrink: 0; align-items: center; justify-content: center; border-radius: 50%; color: white; font-size: 12px; font-weight: 600; }
  .result > div { min-width: 0; flex: 1; }
  p { overflow: hidden; margin: 0; text-overflow: ellipsis; white-space: nowrap; }
  .account { color: var(--muted); font-size: 10px; }
  .subject { color: var(--text); font-size: 13.5px; font-weight: 500; }
  .snippet { color: var(--muted); font-size: 12px; }
</style>

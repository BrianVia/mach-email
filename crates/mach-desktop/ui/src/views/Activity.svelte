<script lang="ts">
  import type { ActivityEntry } from "../lib/ipc";

  let {
    entries,
    onUndo,
  }: {
    entries: ActivityEntry[];
    onUndo: (id: number) => void;
  } = $props();

  const undoable = (entry: ActivityEntry) =>
    !entry.undone && (entry.kind === "modify_labels" || entry.kind === "trash");
</script>

<div class="activity">
  {#if entries.length === 0}
    <p>No recent activity</p>
  {:else}
    {#each entries as entry (entry.id)}
      <div class="row">
        <time>{new Date(entry.at).toLocaleString()}</time>
        <div class="summary">{entry.summary}<small>{entry.account_id}</small></div>
        <span class:failed={entry.state === "failed"} class="state">{entry.undone ? "undone" : entry.state}</span>
        {#if undoable(entry)}<button onclick={() => onUndo(entry.id)}>Undo</button>{/if}
      </div>
    {/each}
  {/if}
</div>

<style>
  .activity { height: 100%; overflow-y: auto; padding: 20px; }
  .row { display: grid; grid-template-columns: 170px minmax(0, 1fr) auto 64px; align-items: center; gap: 14px; min-height: 54px; border-bottom: 1px solid var(--border); }
  time, small, p { color: var(--muted); font-size: 12px; }
  .summary { min-width: 0; font-size: 13px; }
  small { display: block; margin-top: 3px; }
  .state { padding: 3px 8px; border-radius: 999px; background: color-mix(in oklab, var(--accent) 12%, transparent); color: var(--accent); font-size: 11px; }
  .state.failed { color: var(--danger); background: color-mix(in oklab, var(--danger) 12%, transparent); }
  button { padding: 5px 10px; border: 1px solid var(--border); border-radius: 6px; background: var(--surface); color: var(--text); cursor: pointer; }
</style>

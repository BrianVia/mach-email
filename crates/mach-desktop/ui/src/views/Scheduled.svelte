<script lang="ts">
  import type { ScheduledSend } from "../lib/ipc";

  let {
    sends,
    selected,
    onSelect,
    onOpen,
    onCancel,
  }: {
    sends: ScheduledSend[];
    selected: number;
    onSelect: (index: number) => void;
    onOpen: (index: number) => void;
    onCancel: (index: number) => void;
  } = $props();

  function when(iso: string) {
    return new Date(iso).toLocaleString([], {
      weekday: "short",
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
    });
  }
</script>

<div class="list" role="list">
  {#if sends.length === 0}
    <div class="empty">Nothing scheduled.</div>
  {:else}
    {#each sends as send, index (send.send_later_id)}
      <div class:selected={index === selected} class="row">
        <button type="button" class="open" onclick={() => { onSelect(index); onOpen(index); }}>
          <span class="to">{send.to.join(", ") || "(no recipient)"}</span>
          <span class="subject">{send.subject || "(no subject)"}</span>
          <time datetime={send.send_at}>{when(send.send_at)}</time>
        </button>
        <button type="button" class="cancel" onclick={(event) => { event.stopPropagation(); onCancel(index); }}>Cancel</button>
      </div>
    {/each}
  {/if}
</div>

<style>
  .list { height: 100%; overflow-y: auto; }
  .empty { display: grid; height: 100%; place-items: center; color: var(--muted); }
  .row { display: grid; width: 100%; height: 48px; grid-template-columns: 1fr auto; align-items: center; gap: 16px; padding: 0 20px; border-left: 2px solid transparent; color: var(--text); }
  .row:hover { background: var(--hover); }
  .row.selected { border-left-color: var(--accent); background: color-mix(in oklab, var(--accent) 12%, transparent); }
  .open { display: grid; min-width: 0; height: 100%; grid-template-columns: minmax(10rem, 1fr) minmax(14rem, 2fr) 13rem; align-items: center; gap: 16px; border: 0; background: transparent; color: inherit; text-align: left; }
  .to, .subject { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .to, time { color: var(--muted); font-size: 12px; }
  .cancel { padding: 5px 9px; border: 1px solid var(--border); border-radius: 999px; background: transparent; color: var(--danger); }
</style>

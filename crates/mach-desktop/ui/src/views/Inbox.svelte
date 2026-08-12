<script lang="ts">
  import type { ThreadSummary } from "../lib/ipc";
  import { avatarColor, initialsFor } from "../lib/avatar";
  import Icon from "../lib/Icon.svelte";

  const ROW_H = 64;

  let {
    v,
    onSelect,
    onOpen,
  }: {
    v: { kind: "inbox"; label: string; threads: ThreadSummary[]; selected: number };
    onSelect: (index: number) => void;
    onOpen: (index: number) => void;
  } = $props();

  let highlight = $state<HTMLDivElement>();
  let scroller = $state<HTMLDivElement>();

  $effect(() => {
    const index = v.selected;
    if (!highlight) return;
    highlight.style.transform = `translateY(${index * ROW_H}px)`;
    if (!scroller) return;
    const top = index * ROW_H;
    const bottom = top + ROW_H;
    const viewTop = scroller.scrollTop;
    const viewBottom = viewTop + scroller.clientHeight;
    if (top < viewTop) scroller.scrollTo({ top, behavior: "instant" as ScrollBehavior });
    else if (bottom > viewBottom) {
      scroller.scrollTo({ top: bottom - scroller.clientHeight, behavior: "instant" as ScrollBehavior });
    }
  });

  function prettySender(raw: string): string {
    const match = raw.match(/^\s*"?([^"<]+?)"?\s*<.+>$/);
    if (match) return match[1].trim();
    const email = raw.match(/^([\w.+-]+)@([\w.-]+)$/);
    if (email) return email[2];
    return raw;
  }

  function prettyDate(iso: string): string {
    const date = new Date(iso);
    const diff = Date.now() - date.getTime();
    const minute = 60 * 1000;
    const hour = 60 * minute;
    const day = 24 * hour;
    if (diff < day) return date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
    if (diff < 7 * day) return date.toLocaleDateString([], { weekday: "short" });
    if (diff < 365 * day) return date.toLocaleDateString([], { month: "short", day: "numeric" });
    return date.toLocaleDateString([], { year: "numeric" });
  }
</script>

<div bind:this={scroller} class="list" role="list">
  {#if v.threads.length === 0}
    <div class="empty"><span>Inbox zero</span><small>Nothing in this view.</small></div>
  {:else}
    <div bind:this={highlight} class="highlight" style:height={`${ROW_H}px`}></div>
    {#each v.threads as thread, index (thread.id)}
      <button
        type="button"
        class:selected={index === v.selected}
        class="row"
        style:height={`${ROW_H}px`}
        onclick={() => {
          onSelect(index);
          onOpen(index);
        }}
      >
        <div class="indicators">
          {#if thread.unread}<span class="unread" aria-label="unread"></span>{:else}<span class="spacer"></span>{/if}
          {#if thread.starred}<Icon name="star" size={10} class="star" ariaLabel="starred" />{/if}
        </div>
        <span class="avatar" style:background={avatarColor(thread.participants[0] ?? "(no sender)")} aria-hidden="true">
          {initialsFor(thread.participants[0] ?? "(no sender)")}
        </span>
        <div class="content">
          <div class="line">
            <span class="account">{thread.account_id}</span>
            <span class:unread-text={thread.unread} class="sender">{prettySender(thread.participants[0] ?? "(no sender)")}</span>
            <span class="summary">
              <span class:unread-text={thread.unread}>{thread.subject || "(no subject)"}</span>
              <span class="snippet"> — {thread.snippet}</span>
            </span>
          </div>
        </div>
        <div class:unread-date={thread.unread} class="date">{prettyDate(thread.last_message_at)}</div>
      </button>
    {/each}
  {/if}
</div>

<style>
  .list { position: relative; height: 100%; overflow-y: auto; }
  .empty { display: flex; height: 100%; flex-direction: column; align-items: center; justify-content: center; gap: 8px; color: var(--muted); user-select: none; }
  .empty span { font-size: 20px; }
  .empty small { font-size: 14px; }
  .highlight { position: absolute; top: 0; right: 0; left: 0; pointer-events: none; background: color-mix(in oklab, var(--accent) 12%, transparent); border-left: 2px solid var(--accent); transition: transform 90ms cubic-bezier(.22,1,.36,1); will-change: transform; }
  .row { position: relative; display: flex; width: 100%; align-items: center; gap: 12px; padding: 0 20px; border: 0; background: transparent; color: inherit; text-align: left; cursor: pointer; user-select: none; transition: background 150ms; }
  .row:not(.selected):hover { background: var(--hover); }
  .indicators { display: flex; width: 12px; flex-shrink: 0; flex-direction: column; align-items: center; gap: 4px; }
  .unread, .spacer { width: 8px; height: 8px; }
  .unread { border-radius: 50%; background: var(--accent); }
  :global(.star) { color: var(--starred); }
  .avatar { display: inline-flex; width: 32px; height: 32px; flex-shrink: 0; align-items: center; justify-content: center; border-radius: 50%; color: white; font-size: 12px; font-weight: 600; letter-spacing: .02em; }
  .content { min-width: 0; flex: 1; }
  .line { display: flex; min-width: 0; align-items: baseline; gap: 8px; }
  .account { flex-shrink: 0; padding: 2px 6px; border-radius: 4px; background: var(--surface-2); color: var(--muted); font-size: 10px; }
  .sender { max-width: 30%; flex-shrink: 0; overflow: hidden; color: var(--muted); font-size: 14px; font-weight: 500; text-overflow: ellipsis; white-space: nowrap; }
  .summary { min-width: 0; overflow: hidden; color: var(--text); text-overflow: ellipsis; white-space: nowrap; }
  .snippet { color: var(--muted); }
  .unread-text { color: var(--text); font-weight: 600; }
  .date { flex-shrink: 0; color: var(--muted); font-size: 12px; font-variant-numeric: tabular-nums; text-align: right; }
  .unread-date { color: var(--text); }
</style>

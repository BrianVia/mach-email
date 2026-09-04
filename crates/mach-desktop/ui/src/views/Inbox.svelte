<script lang="ts">
  import type { ThreadSummary } from "../lib/ipc";
  import { splitOf, type Split } from "../lib/split";
  import { avatarColor } from "../lib/avatar";
  import Icon from "../lib/Icon.svelte";

  const ROW_H = 44;
  const HEADER_H = 28;
  const DAY_MS = 24 * 60 * 60 * 1000;

  type RenderRow =
    | { kind: "header"; label: string }
    | { kind: "thread"; thread: ThreadSummary; index: number };

  let {
    v,
    onSelect,
    onOpen,
    split,
    onSplit,
    onLoadOlder,
    accountLabel,
  }: {
    v: { kind: "inbox"; label: string; threads: ThreadSummary[]; selected: number; limit: number };
    onSelect: (index: number) => void;
    onOpen: (index: number) => void;
    split: Split;
    onSplit: (split: Split) => void;
    onLoadOlder: () => Promise<number | null>;
    accountLabel: (email: string) => string;
  } = $props();

  let highlight = $state<HTMLDivElement>();
  let scroller = $state<HTMLDivElement>();
  let scrolledToEnd = $state(false);
  let loadingOlder = $state(false);
  let noOlder = $state(false);
  let previousLabel = $state("");

  $effect(() => {
    if (v.label !== previousLabel) {
      previousLabel = v.label;
      scrolledToEnd = false;
      noOlder = false;
    }
  });

  async function loadMore() {
    if (loadingOlder) return;
    loadingOlder = true;
    const fetched = await onLoadOlder();
    loadingOlder = false;
    if (fetched !== null) noOlder = fetched === 0;
  }

  // ponytail: client-side split is capped by the current list limit; move it into the store if pagination needs category-complete results.
  let threads = $derived(v.label === "INBOX" ? v.threads.filter((thread) => splitOf(thread.label_ids) === split) : v.threads);
  let splits = $derived((["important", "other", "newsletters"] as const).map((candidate) => ({
    value: candidate,
    label: candidate[0].toUpperCase() + candidate.slice(1),
    unread: v.threads.filter((thread) => splitOf(thread.label_ids) === candidate && thread.unread).length,
  })));

  let inboxLayout = $derived.by(() => {
    const now = new Date();
    const renderRows: RenderRow[] = [];
    const rowTop: number[] = [];
    const multipleAccounts = new Set(threads.map((thread) => thread.account_id)).size > 1;
    let offset = 0;
    let previousGroup: string | null = null;

    for (const [index, thread] of threads.entries()) {
      const date = new Date(thread.last_message_at);
      const group = dateGroup(date, now);
      if (group !== null && group !== previousGroup) {
        renderRows.push({ kind: "header", label: group });
        offset += HEADER_H;
      }
      renderRows.push({ kind: "thread", thread, index });
      rowTop[index] = offset;
      offset += ROW_H;
      previousGroup = group;
    }

    return { renderRows, rowTop, multipleAccounts };
  });

  $effect(() => {
    const index = v.selected;
    if (!highlight) return;
    const top = inboxLayout.rowTop[index];
    if (top === undefined) {
      highlight.style.display = "none";
      return;
    }
    highlight.style.display = "";
    highlight.style.transform = `translateY(${top}px)`;
    if (!scroller) return;
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

  function categoryOf(thread: ThreadSummary): { text: string; hue: string } | null {
    for (const label of thread.label_ids) {
      if (label === "CATEGORY_UPDATES") return { text: "updates", hue: "#6b8afd" };
      if (label === "CATEGORY_PROMOTIONS") return { text: "promos", hue: "#e8a13c" };
      if (label === "CATEGORY_SOCIAL") return { text: "social", hue: "#4cb782" };
      if (label === "CATEGORY_FORUMS") return { text: "forums", hue: "#a78bfa" };
    }
    return null;
  }

  function prettyDate(iso: string): string {
    const date = new Date(iso);
    const now = new Date();
    if (isSameDay(date, now)) {
      return date.toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" }).toUpperCase();
    }
    const monthDay = `${date.toLocaleDateString("en-US", { month: "short" }).toUpperCase()} ${date.getDate()}`;
    return date.getFullYear() === now.getFullYear() ? monthDay : `${monthDay}, ${date.getFullYear()}`;
  }

  function isSameDay(left: Date, right: Date): boolean {
    return left.getFullYear() === right.getFullYear()
      && left.getMonth() === right.getMonth()
      && left.getDate() === right.getDate();
  }

  function dateGroup(date: Date, now: Date): string | null {
    if (now.getTime() - date.getTime() < 7 * DAY_MS) return null;
    if (date.getFullYear() === now.getFullYear() && date.getMonth() === now.getMonth()) {
      return "Earlier this month";
    }
    const month = date.toLocaleDateString("en-US", { month: "long" });
    return date.getFullYear() === now.getFullYear() ? month : `${month} ${date.getFullYear()}`;
  }
</script>

{#if v.label === "INBOX"}
  <div class="tabs" aria-label="Inbox category">
    {#each splits as tab}
      <button type="button" class:active={split === tab.value} onclick={() => onSplit(tab.value)}>
        {tab.label} <span>{tab.unread}</span>
      </button>
    {/each}
  </div>
{/if}
<div
  bind:this={scroller}
  class="list"
  class:list-with-tabs={v.label === "INBOX"}
  role="list"
  onscroll={() => {
    if (scroller && scroller.scrollHeight - scroller.scrollTop <= scroller.clientHeight + 1) {
      scrolledToEnd = true;
    }
  }}
>
  {#if threads.length === 0}
    <div class="empty"><span>Inbox zero</span><small>Nothing in this view.</small></div>
  {:else}
    <div bind:this={highlight} class="highlight" style:height={`${ROW_H}px`}></div>
    {#each inboxLayout.renderRows as item (item.kind === "header" ? `h:${item.label}` : item.thread.id)}
      {#if item.kind === "header"}
        <div class="group-header">{item.label}</div>
      {:else}
        {@const category = categoryOf(item.thread)}
        <button
          type="button"
          class:selected={item.index === v.selected}
          class="row"
          style:height={`${ROW_H}px`}
          onclick={() => {
            onSelect(item.index);
            onOpen(item.index);
          }}
        >
          <div class="indicators">
            {#if item.thread.unread}<span class="unread" aria-label="unread"></span>{:else}<span class="spacer"></span>{/if}
            {#if item.thread.starred}<Icon name="star" size={10} class="star" ariaLabel="starred" />{/if}
          </div>
          {#if inboxLayout.multipleAccounts}
            <span
              class="account-marker"
              style:background={avatarColor(item.thread.account_id)}
              title={item.thread.account_id}
              aria-hidden="true"
            ></span>
            <span class="account-text" title={item.thread.account_id}>{accountLabel(item.thread.account_id)}</span>
          {/if}
          <span class:unread-text={item.thread.unread} class="sender">
            {prettySender(item.thread.participants[0] ?? "(no sender)")}
          </span>
          {#if category}
            <span class="badge" style:--badge={category.hue}>{category.text}</span>
          {/if}
          <span class="summary">
            <span class:unread-text={item.thread.unread} class="subject">{item.thread.subject || "(no subject)"}</span>
            <span class="snippet">{item.thread.snippet}</span>
          </span>
          <div class:unread-date={item.thread.unread} class="date">{prettyDate(item.thread.last_message_at)}</div>
        </button>
      {/if}
    {/each}
    {#if v.threads.length >= v.limit || scrolledToEnd || noOlder}
      <button class="load-older" type="button" disabled={loadingOlder} onclick={loadMore}>
        {loadingOlder ? "Loading…" : noOlder ? "No older mail" : "Load older…"}
      </button>
    {/if}
  {/if}
</div>

<style>
  .list { position: relative; height: 100%; overflow-y: auto; }
  .list-with-tabs { height: calc(100% - 36px); }
  .tabs { box-sizing: border-box; display: flex; height: 36px; align-items: end; gap: 18px; padding: 0 16px 7px; border-bottom: 1px solid var(--border); font-size: 12px; }
  .tabs button { padding: 0; border: 0; background: transparent; color: var(--muted); font: inherit; cursor: pointer; }
  .tabs button.active { color: var(--accent); font-weight: 600; }
  .tabs span { margin-left: 3px; color: var(--muted); font-variant-numeric: tabular-nums; }
  .empty { display: flex; height: 100%; flex-direction: column; align-items: center; justify-content: center; gap: 8px; color: var(--muted); user-select: none; }
  .empty span { font-size: 20px; }
  .empty small { font-size: 14px; }
  .highlight { position: absolute; top: 0; right: 0; left: 0; pointer-events: none; background: color-mix(in oklab, var(--accent) 12%, transparent); border-left: 2px solid var(--accent); transition: transform 90ms cubic-bezier(.22,1,.36,1); will-change: transform; }
  .group-header { box-sizing: border-box; height: 28px; padding: 12px 0 4px 42px; color: var(--muted); font-size: 11px; font-weight: 600; letter-spacing: .06em; line-height: 12px; user-select: none; }
  .row { position: relative; display: flex; width: 100%; align-items: center; gap: 12px; padding: 0 24px 0 16px; border: 0; background: transparent; color: inherit; text-align: left; cursor: pointer; user-select: none; transition: background 150ms; }
  .row:not(.selected):hover { background: var(--hover); }
  .indicators { display: flex; width: 14px; flex-shrink: 0; flex-direction: column; align-items: center; gap: 4px; }
  .unread, .spacer { width: 8px; height: 8px; }
  .unread { border-radius: 50%; background: var(--accent); }
  :global(.star) { color: var(--starred); }
  .account-marker { width: 6px; height: 6px; flex-shrink: 0; border-radius: 50%; }
  .account-text { max-width: 90px; overflow: hidden; color: var(--muted); font-size: 10px; text-overflow: ellipsis; white-space: nowrap; }
  .sender { width: 13rem; flex-shrink: 0; overflow: hidden; color: color-mix(in oklab, var(--text) 78%, transparent); font-size: 13.5px; font-weight: 400; text-overflow: ellipsis; white-space: nowrap; }
  .badge { flex-shrink: 0; padding: 1px 6px; border-radius: 4px; background: color-mix(in oklab, var(--badge) 14%, transparent); color: color-mix(in oklab, var(--badge) 80%, var(--text)); font-size: 10px; font-weight: 500; letter-spacing: .02em; }
  .summary { min-width: 0; flex: 1; overflow: hidden; color: color-mix(in oklab, var(--text) 78%, transparent); font-size: 13.5px; font-weight: 400; text-overflow: ellipsis; white-space: nowrap; }
  .snippet { margin-left: 12px; color: var(--muted); font-weight: 400; }
  .unread-text { color: var(--text); font-weight: 600; }
  .date { min-width: 64px; flex-shrink: 0; color: var(--muted); font-size: 12px; font-variant-numeric: tabular-nums; text-align: right; }
  .unread-date { color: var(--accent); font-weight: 600; }
  .load-older { display: block; width: 100%; height: 44px; border: 0; background: transparent; color: var(--muted); cursor: pointer; }
  .load-older:hover:not(:disabled) { background: var(--hover); }
</style>

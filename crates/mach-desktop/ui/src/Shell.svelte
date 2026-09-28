<script lang="ts">
  import type { Snippet } from "svelte";
  import type { OutboxSummary } from "./lib/ipc";
  import SidebarItem from "./aurora/SidebarItem.svelte";

  type Continuation = { next: string; action_name: string };

  let {
    title,
    subtitle,
    accountEmail,
    accountLabel,
    accounts,
    accountFilter,
    onAccountFilter,
    onAddAccount,
    online,
    outbox,
    activeLabel,
    chordBuf,
    chordConts,
    userLabels,
    onOpenLabel,
    onOpenActivity,
    onRetryFailed,
    children,
  }: {
    title: string;
    subtitle: string;
    accountEmail?: string;
    accountLabel?: string;
    accounts: { email: string; label: string }[];
    accountFilter: string | null;
    // Undefined when the current view has no inbox to filter.
    onAccountFilter?: (email: string | null) => void;
    onAddAccount: () => void;
    online: boolean;
    outbox: OutboxSummary;
    activeLabel?: string;
    chordBuf: string;
    chordConts: Continuation[];
    userLabels: { id: string; ids: string[]; name: string; unread_count: number | null }[];
    onOpenLabel: (label: string) => void;
    onOpenActivity: () => void;
    onRetryFailed: () => void;
    children: Snippet;
  } = $props();

  let labelsCollapsed = $state((() => {
    try { return localStorage.getItem("mach.labelsCollapsed") === "1"; } catch { return false; }
  })());

  const labels = [
    ["Inbox", "INBOX"],
    ["Starred", "STARRED"],
    ["Sent", "SENT"],
    ["Drafts", "DRAFT"],
    ["Scheduled", "SCHEDULED"],
    ["Done", "DONE"],
    ["Snoozed", "SNOOZED"],
    ["Trash", "TRASH"],
    ["Spam", "SPAM"],
    ["All Mail", "ALL"],
  ] as const;
</script>

<div class="shell">
  <header data-tauri-drag-region>
    <div class="heading" data-tauri-drag-region>
      <div class="title" data-tauri-drag-region>{title}</div>
      <div class="subtitle" data-tauri-drag-region>{subtitle}</div>
    </div>
    <div class="account">
      {#if accounts.length > 1}
        <span class="account-picker">
          <select
            aria-label="Filter inbox by account"
            value={accountFilter ?? ""}
            disabled={!onAccountFilter}
            onchange={(event) => {
              onAccountFilter?.(event.currentTarget.value || null);
              // App shortcuts listen on document; drop focus so j/k don't also move the select.
              event.currentTarget.blur();
            }}
          >
            <option value="">{accountLabel}</option>
            {#each accounts as account}
              <option value={account.email} title={account.email}>{account.label === account.email ? account.label : `${account.label} (${account.email})`}</option>
            {/each}
          </select><i aria-hidden="true">▾</i>
        </span>
        {#if accountFilter && onAccountFilter}
          <button class="clear-account" type="button" aria-label="Show all accounts" title="Show all accounts" onclick={() => onAccountFilter(null)}>✕</button>
        {/if}
      {:else}
        <span title={accountEmail}>{accountLabel}</span>
      {/if}
      <button class="add-account" type="button" aria-label="Add account" title="Add account" onclick={onAddAccount}>+</button>
      <span class:online class="status-pill">
        <i></i>{online ? "Live" : "Offline"}
      </span>
      {#if outbox.failed > 0}
        <button type="button" class="status-pill failed" title={`${outbox.last_error ?? ""}\nClick to retry`.trim()} onclick={onRetryFailed}>{outbox.failed} failed</button>
      {:else if outbox.pending > 0}
        <span class="status-pill unsynced">{outbox.pending} unsynced</span>
      {/if}
    </div>
  </header>

  <div class="body">
    <aside>
      <div class="brand">mach</div>
      {#each labels as [name, id]}
        <SidebarItem active={activeLabel === id} onclick={() => onOpenLabel(id)}>
          {name}
        </SidebarItem>
      {/each}
      <details
        open={!labelsCollapsed}
        ontoggle={(event) => {
          labelsCollapsed = !event.currentTarget.open;
          try { localStorage.setItem("mach.labelsCollapsed", labelsCollapsed ? "1" : "0"); } catch {}
        }}
      >
        <summary class="section-header">Labels</summary>
        {#each userLabels as label}
          <SidebarItem active={activeLabel !== undefined && label.ids.includes(activeLabel)} onclick={() => onOpenLabel(label.id)}>
            <span class="label-row"><span>{label.name}</span>{#if label.unread_count}<span>{label.unread_count}</span>{/if}</span>
          </SidebarItem>
        {/each}
      </details>
      <div class="sidebar-spacer"></div>
      <SidebarItem active={activeLabel === "ACTIVITY"} onclick={onOpenActivity}>Activity</SidebarItem>
      <div class="sidebar-hint">g then i/s/t/d/e/z/k/j/a</div>
    </aside>
    <main>{@render children()}</main>
  </div>

  <footer>
    {#if chordBuf}
      <span class="chord">{chordBuf}</span><span>→</span>
      <div class="continuations">
        {#each chordConts as continuation}
          <span class="hint"><kbd>{continuation.next}</kbd>{continuation.action_name.replace(/_/g, " ")}</span>
        {/each}
      </div>
    {:else}
      <div class="continuations">
        <span class="hint"><kbd>j/k</kbd>nav</span>
        <span class="hint"><kbd>e</kbd>archive</span>
        <span class="hint"><kbd>r</kbd>reply</span>
        <span class="hint"><kbd>c</kbd>compose</span>
        <span class="hint"><kbd>/</kbd>search</span>
        <span class="hint"><kbd>esc</kbd>back</span>
      </div>
    {/if}
  </footer>
</div>

<style>
  .shell { width: 100vw; height: 100vh; display: grid; grid-template-rows: 44px 1fr 34px; background: var(--surface); }
  header { min-width: 0; display: flex; align-items: center; padding-left: 78px; padding-right: 20px; border-bottom: 1px solid var(--border); user-select: none; }
  .heading { flex: 1; min-width: 0; }
  .title { overflow: hidden; color: var(--text); font-size: 13px; font-weight: 600; line-height: 17px; text-overflow: ellipsis; white-space: nowrap; }
  .subtitle { overflow: hidden; color: var(--muted); font-size: 11px; line-height: 14px; text-overflow: ellipsis; white-space: nowrap; }
  .account { display: flex; flex-shrink: 0; align-items: center; gap: 8px; color: var(--muted); font-size: 11px; }
  .account-picker { position: relative; display: inline-flex; align-items: center; }
  .account-picker select { appearance: none; padding: 0 12px 0 0; border: 0; background: transparent; color: inherit; cursor: pointer; font: inherit; }
  .account-picker select:disabled { cursor: default; }
  .account-picker i { position: absolute; right: 0; font-style: normal; pointer-events: none; }
  .clear-account { padding: 0 2px; border: 0; background: transparent; color: var(--muted); cursor: pointer; font: inherit; }
  .add-account { padding: 0 3px; border: 0; background: transparent; color: var(--muted); cursor: pointer; font: inherit; font-size: 15px; }
  .status-pill { display: inline-flex; align-items: center; gap: 6px; padding: 2px 8px; border-radius: 999px; color: var(--muted); background: color-mix(in oklab, var(--muted) 14%, transparent); font-weight: 500; }
  .status-pill i { width: 6px; height: 6px; border-radius: 50%; background: var(--muted); }
  .status-pill.online { color: var(--success); background: color-mix(in oklab, var(--success) 14%, transparent); }
  .status-pill.online i { background: var(--success); box-shadow: 0 0 6px color-mix(in oklab, var(--success) 60%, transparent); }
  .status-pill.unsynced { color: #d99500; background: color-mix(in oklab, #d99500 14%, transparent); }
  .status-pill.failed { color: var(--danger); background: color-mix(in oklab, var(--danger) 14%, transparent); }
  button.status-pill { border: 0; font: inherit; cursor: pointer; }
  .body { display: grid; min-height: 0; grid-template-columns: 210px 1fr; }
  aside { display: flex; min-height: 0; flex-direction: column; padding: 14px; border-right: 1px solid var(--border); background: var(--sidebar); }
  .brand { padding: 8px 9px 18px; font-weight: 750; }
  .section-header { padding: 18px 9px 6px; color: var(--muted); font-size: 11px; font-weight: 600; }
  summary.section-header { cursor: pointer; list-style: none; user-select: none; }
  summary.section-header::-webkit-details-marker { display: none; }
  summary.section-header::before { content: "▾"; display: inline-block; width: 12px; }
  details:not([open]) > summary.section-header::before { content: "▸"; }
  .label-row { display: flex; width: 100%; justify-content: space-between; gap: 8px; }
  .sidebar-hint { margin: 16px 9px 0; color: var(--muted); font-size: 11px; line-height: 1.45; }
  .sidebar-spacer { flex: 1; }
  main { position: relative; min-width: 0; min-height: 0; overflow: hidden; background: var(--bg); }
  footer { display: flex; align-items: center; gap: 10px; min-width: 0; padding: 0 20px; border-top: 1px solid var(--border); color: var(--muted); font-size: 11px; user-select: none; }
  .chord { margin-right: 2px; color: var(--accent); font-family: var(--font-mono); }
  .continuations { display: flex; align-items: center; gap: 16px; min-width: 0; }
  .hint { display: inline-flex; align-items: center; gap: 6px; white-space: nowrap; }
  kbd { padding: 2px 5px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-2); color: var(--text); font-family: var(--font-mono); font-size: 11px; line-height: 1; }
</style>

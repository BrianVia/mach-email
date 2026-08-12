<script lang="ts">
  import { onMount } from "svelte";
  import { Keymap, keyEventToChord, type KeyContext, type Mode } from "./lib/keymap";
  import {
    dispatchAction,
    fetchAccountStatus,
    fetchKeymapSources,
    flushOutbox,
    listThreads,
    openThread as openThreadIpc,
    refetchThread as refetchThreadIpc,
    searchThreads,
    type AccountStatus,
    type ActionOutcome,
    type Draft,
    type Message,
    type ThreadSummary,
  } from "./lib/ipc";
  import Shell from "./Shell.svelte";
  import Inbox from "./views/Inbox.svelte";
  import ThreadReader from "./views/Thread.svelte";
  import Composer from "./views/Composer.svelte";
  import SearchOverlay from "./views/Search.svelte";
  import ChordOverlay from "./views/ChordOverlay.svelte";

  const INBOX_LIMIT = 1000;

  type InboxView = { kind: "inbox"; label: string; threads: ThreadSummary[]; selected: number };
  type ThreadView = { kind: "thread"; thread: ThreadSummary; messages: Message[]; selectedMsg: number };
  type ComposerFields = { to: string; cc: string; subject: string; body_md: string };
  type ComposerView = { kind: "composer"; draft: Draft; background: AppView };
  type SearchView = { kind: "search"; query: string; results: ThreadSummary[]; selected: number; background: AppView };
  type AppView = InboxView | ThreadView | ComposerView | SearchView;
  type Continuation = { next: string; action_name: string };

  let view = $state<AppView>({ kind: "inbox", label: "INBOX", threads: [], selected: 0 });
  let keymap = $state<Keymap | null>(null);
  let bootError = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let chordBuf = $state("");
  let chordConts = $state<Continuation[]>([]);
  let status = $state<AccountStatus | null>(null);
  let composerFields: ComposerFields = { to: "", cc: "", subject: "", body_md: "" };
  let actionErrorTimer: number | undefined;
  let noticeTimer: number | undefined;

  let title = $derived.by(() => {
    if (view.kind === "inbox") return labelDisplay(view.label);
    if (view.kind === "thread") return view.thread.subject || "(no subject)";
    if (view.kind === "composer") return "New message";
    return "Search";
  });

  let subtitle = $derived.by(() => {
    if (view.kind === "inbox") return `${view.threads.length.toLocaleString()} threads`;
    if (view.kind === "thread") {
      return `${view.thread.participants.slice(0, 2).join(", ")}${view.thread.participants.length > 2 ? ` +${view.thread.participants.length - 2}` : ""}`;
    }
    return status?.email ?? "";
  });

  let activeLabel = $derived(view.kind === "inbox" ? view.label : undefined);

  function showActionError(error: unknown) {
    actionError = String((error as Error).message ?? error);
    if (actionErrorTimer !== undefined) window.clearTimeout(actionErrorTimer);
    actionErrorTimer = window.setTimeout(() => (actionError = null), 4_000);
  }

  function showNotice(message: string | null) {
    notice = message;
    if (noticeTimer !== undefined) window.clearTimeout(noticeTimer);
    if (message) noticeTimer = window.setTimeout(() => (notice = null), 4_000);
  }

  async function boot() {
    try {
      const sources = await fetchKeymapSources();
      try {
        const resolved = Keymap.fromToml(sources.defaults);
        if (sources.user) resolved.merge(Keymap.fromToml(sources.user));
        keymap = resolved;
      } catch (error) {
        console.error("[mach] keymap parse failed:", error);
        bootError = `keymap parse: ${(error as Error).message ?? error}`;
      }
      const threads = await listThreads("INBOX", INBOX_LIMIT);
      console.log(`[mach] loaded ${threads.length} threads`);
      view = { kind: "inbox", label: "INBOX", threads, selected: 0 };
    } catch (error) {
      console.error("[mach]", error);
      bootError = `boot failed: ${(error as Error).message ?? error}`;
    }
  }

  async function refreshStatus() {
    try {
      status = await fetchAccountStatus();
    } catch (error) {
      console.warn("[mach] account status failed", error);
    }
  }

  function handleKey(event: KeyboardEvent) {
    if (!keymap) {
      console.warn("[mach] keymap not loaded yet; key dropped", event.key);
      return;
    }

    const currentView = view;
    const target = event.target as HTMLElement | null;
    const inTextInput = target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);
    if (inTextInput && !isControlKey(event)) return;
    if ((currentView.kind === "composer" || currentView.kind === "search") && !isControlKey(event)) return;

    const chord = keyEventToChord(event);
    if (!chord) return;
    const newBuf = chordBuf ? `${chordBuf} ${chord}` : chord;
    const context = currentContext(currentView);
    const mode = currentMode(currentView);
    const resolution = keymap.resolve(mode, newBuf, context);
    console.debug("[mach] key", { chord: newBuf, mode, resolution: resolution.kind });

    if (resolution.kind === "action") {
      event.preventDefault();
      chordBuf = "";
      chordConts = [];
      void runAction(resolution.action);
    } else if (resolution.kind === "prefix") {
      event.preventDefault();
      chordBuf = newBuf;
      chordConts = resolution.continuations;
    } else if (chordBuf) {
      chordBuf = "";
      chordConts = [];
      const fresh = keymap.resolve(mode, chord, context);
      if (fresh.kind === "action") {
        event.preventDefault();
        void runAction(fresh.action);
      }
    }
  }

  function currentContext(currentView: AppView): KeyContext {
    if (currentView.kind === "inbox") {
      const thread = currentView.threads[currentView.selected];
      return { selection: thread ? [thread.id] : [], current_thread: thread?.id };
    }
    if (currentView.kind === "thread") {
      const message = currentView.messages[currentView.selectedMsg];
      return { selection: [currentView.thread.id], current_thread: currentView.thread.id, current_message: message?.id };
    }
    if (currentView.kind === "search") {
      const thread = currentView.results[currentView.selected];
      return { selection: thread ? [thread.id] : [], current_thread: thread?.id };
    }
    return { selection: [], current_draft: currentView.draft.id };
  }

  function currentMode(currentView: AppView): Mode {
    switch (currentView.kind) {
      case "inbox": return "normal";
      case "thread": return "reading";
      case "composer": return "composing";
      case "search": return "search";
    }
  }

  async function runAction(action: Record<string, unknown>) {
    const kind = action.kind as string;
    const currentView = view;
    try {
      switch (kind) {
        case "quit":
          return;
        case "back_to_list": {
          if (currentView.kind === "composer") {
            closeComposer();
            return;
          }
          const threads = await listThreads("INBOX", INBOX_LIMIT);
          view = { kind: "inbox", label: "INBOX", threads, selected: 0 };
          return;
        }
        case "select_next":
          moveSelection(1);
          return;
        case "select_prev":
          moveSelection(-1);
          return;
        case "compose_new":
        case "reply":
        case "forward": {
          const outcome = await dispatchAction(action);
          openComposer(draftFromOutcome(outcome), currentView);
          return;
        }
        case "save_draft": {
          if (currentView.kind !== "composer") return;
          const draft = await saveComposerDraft(currentView, action);
          if (view.kind === "composer" && view.draft.id === draft.id) {
            view = { ...view, draft };
          }
          return;
        }
        case "send_draft": {
          if (currentView.kind !== "composer") return;
          const draft = await saveComposerDraft(currentView, action);
          if (view.kind === "composer" && view.draft.id === draft.id) {
            view = { ...view, draft };
          }
          await dispatchAction({ kind: "send_draft", draft_id: draft.id });
          view = currentView.background;
          showNotice("Sending…");
          const report = await flushOutbox();
          if (report.failed > 0) {
            showNotice(null);
            showActionError(report.last_error ?? `${report.failed} outbox operation(s) failed`);
          } else {
            showNotice("Sent");
          }
          return;
        }
        case "open_thread": {
          const id = (action.id as string) ?? "";
          const thread = currentView.kind === "inbox"
            ? currentView.threads.find((candidate) => candidate.id === id)
            : currentView.kind === "search"
              ? currentView.results.find((candidate) => candidate.id === id)
              : undefined;
          if (!thread) throw new Error("thread not found");
          await openThreadView(thread);
          return;
        }
        case "open_label": {
          const label = action.label_id as string;
          const threads = await listThreads(label, INBOX_LIMIT);
          view = { kind: "inbox", label, threads, selected: 0 };
          return;
        }
        case "search":
          view = { kind: "search", query: "", results: [], selected: 0, background: currentView };
          return;
        case "refresh": {
          if (currentView.kind === "thread") {
            const opened = await refetchThreadIpc(currentView.thread.id);
            view = { kind: "thread", thread: opened.thread, messages: opened.messages, selectedMsg: currentView.selectedMsg };
          } else if (currentView.kind === "inbox") {
            const threads = await listThreads(currentView.label, INBOX_LIMIT);
            view = { ...currentView, threads };
          }
          return;
        }
      }

      const outcome = await dispatchAction(action);
      const removedSet = new Set(outcome.changed_threads);
      const isArchiveOrTrash = kind === "archive" || kind === "trash";

      if (isArchiveOrTrash && currentView.kind === "inbox" && removedSet.size) {
        const threads = currentView.threads.filter((thread) => !removedSet.has(thread.id));
        view = { ...currentView, threads, selected: Math.min(currentView.selected, Math.max(0, threads.length - 1)) };
      }

      if (isArchiveOrTrash && currentView.kind === "thread") {
        const threads = await listThreads("INBOX", INBOX_LIMIT);
        const at = threads.findIndex((thread) => thread.id === currentView.thread.id);
        const fallback = at >= 0 ? Math.min(at, threads.length - 1) : 0;
        view = { kind: "inbox", label: "INBOX", threads, selected: fallback };
      }

      if (!isArchiveOrTrash && currentView.kind === "inbox" && removedSet.size) {
        const selectedId = currentView.threads[currentView.selected]?.id;
        const threads = await listThreads(currentView.label, INBOX_LIMIT);
        const preserved = selectedId ? threads.findIndex((thread) => thread.id === selectedId) : -1;
        view = {
          ...currentView,
          threads,
          selected: preserved >= 0 ? preserved : clamp(currentView.selected, 0, threads.length - 1),
        };
      }
    } catch (error) {
      console.warn("dispatch failed", error);
      if (kind === "send_draft") showNotice(null);
      showActionError(error);
    }
  }

  async function openThreadView(thread: ThreadSummary) {
    const opened = await openThreadIpc(thread.id);
    view = { kind: "thread", thread: opened.thread, messages: opened.messages, selectedMsg: 0 };
    if (thread.unread) {
      void dispatchAction({ kind: "mark_read", thread_ids: [thread.id], read: true }).catch(() => {});
    }
  }

  function openComposer(draft: Draft, background: AppView) {
    composerFields = fieldsFromDraft(draft);
    view = { kind: "composer", draft, background };
  }

  function draftFromOutcome(outcome: ActionOutcome): Draft {
    const draft = (outcome.data as { draft?: Draft } | null)?.draft;
    if (!draft) throw new Error("draft action returned no draft");
    return draft;
  }

  function fieldsFromDraft(draft: Draft): ComposerFields {
    return {
      to: draft.to.join(", "),
      cc: draft.cc.join(", "),
      subject: draft.subject,
      body_md: draft.body_md,
    };
  }

  function draftPatch() {
    return {
      to: parseRecipients(composerFields.to),
      cc: parseRecipients(composerFields.cc),
      subject: composerFields.subject,
      body_md: composerFields.body_md,
    };
  }

  async function saveComposerDraft(
    composer: ComposerView,
    action: Record<string, unknown> = { kind: "save_draft", draft_id: composer.draft.id },
  ): Promise<Draft> {
    const outcome = await dispatchAction({
      ...action,
      kind: "save_draft",
      draft_id: composer.draft.id,
      patch: draftPatch(),
    });
    return draftFromOutcome(outcome);
  }

  function parseRecipients(value: string): string[] {
    return value.split(",").map((recipient) => recipient.trim()).filter(Boolean);
  }

  function moveSelection(delta: number) {
    const currentView = view;
    if (currentView.kind === "inbox") {
      view = { ...currentView, selected: clamp(currentView.selected + delta, 0, currentView.threads.length - 1) };
    } else if (currentView.kind === "thread") {
      view = { ...currentView, selectedMsg: clamp(currentView.selectedMsg + delta, 0, currentView.messages.length - 1) };
    } else if (currentView.kind === "search") {
      view = { ...currentView, selected: clamp(currentView.selected + delta, 0, currentView.results.length - 1) };
    }
  }

  async function onSearchInput(query: string) {
    const currentView = view;
    if (currentView.kind !== "search") return;
    try {
      const trimmed = query.trim();
      const results = trimmed ? await searchThreads(trimmed, 50) : [];
      view = { ...currentView, query, results, selected: 0 };
    } catch (error) {
      console.warn("search failed", error);
      showActionError(error);
    }
  }

  async function onSearchEnter() {
    const currentView = view;
    if (currentView.kind !== "search") return;
    const thread = currentView.results[currentView.selected];
    if (!thread) return;
    try {
      await openThreadView(thread);
    } catch (error) {
      console.warn("open thread failed", error);
      showActionError(error);
    }
  }

  function onSearchEsc() {
    if (view.kind === "search") view = view.background;
  }

  function closeComposer() {
    const currentView = view;
    if (currentView.kind !== "composer") return;
    void saveComposerDraft(currentView).catch((error) => {
      console.warn("final draft save failed", error);
      showActionError(error);
    });
    view = currentView.background;
  }

  async function openInboxRow(index: number) {
    if (view.kind !== "inbox") return;
    const thread = view.threads[index];
    if (!thread) return;
    try {
      await openThreadView(thread);
    } catch (error) {
      console.warn("open thread failed", error);
      showActionError(error);
    }
  }

  onMount(() => {
    void boot();
    void refreshStatus();
    const statusTimer = window.setInterval(() => void refreshStatus(), 30_000);
    document.addEventListener("keydown", handleKey, true);
    window.focus();
    document.body.tabIndex = -1;
    document.body.focus();

    return () => {
      window.clearInterval(statusTimer);
      document.removeEventListener("keydown", handleKey, true);
      if (actionErrorTimer !== undefined) window.clearTimeout(actionErrorTimer);
      if (noticeTimer !== undefined) window.clearTimeout(noticeTimer);
    };
  });

  function clamp(value: number, low: number, high: number) {
    if (high < low) return low;
    return Math.min(Math.max(value, low), high);
  }

  function isControlKey(event: KeyboardEvent) {
    if (event.key === "Escape") return true;
    if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) return true;
    return event.key === "s" && (event.ctrlKey || event.metaKey);
  }

  function labelDisplay(id: string) {
    const names: Record<string, string> = {
      INBOX: "Inbox", STARRED: "Starred", SENT: "Sent", DRAFT: "Drafts", TRASH: "Trash", SPAM: "Spam", DONE: "Done",
    };
    return names[id] ?? id;
  }
</script>

<Shell
  {title}
  {subtitle}
  accountEmail={status?.email}
  online={status?.online ?? false}
  {activeLabel}
  {chordBuf}
  {chordConts}
  onOpenLabel={(label) => void runAction({ kind: "open_label", label_id: label })}
>
  {#if view.kind === "inbox"}
    <Inbox
      v={view}
      onSelect={(selected) => {
        if (view.kind === "inbox") view = { ...view, selected };
      }}
      onOpen={(index) => void openInboxRow(index)}
    />
  {:else if view.kind === "thread"}
    <ThreadReader v={view} />
  {:else if view.kind === "composer"}
    <Composer
      draft={view.draft}
      onFieldsChange={(fields) => (composerFields = fields)}
      onSend={() => void runAction({ kind: "send_draft", draft_id: view.kind === "composer" ? view.draft.id : "" })}
      onClose={closeComposer}
    />
  {:else if view.kind === "search"}
    <SearchOverlay v={view} onInput={onSearchInput} onEnter={onSearchEnter} onEsc={onSearchEsc} onMove={moveSelection} />
  {/if}

  {#if bootError || actionError || notice}
    <div class="errors">
      {#if bootError}<div class="error">⚠ {bootError}</div>{/if}
      {#if actionError}<button class="error" onclick={() => (actionError = null)}>⚠ {actionError}</button>{/if}
      {#if notice}<button class="notice" onclick={() => showNotice(null)}>{notice}</button>{/if}
    </div>
  {/if}
  <ChordOverlay show={chordBuf.length > 0} buf={chordBuf} conts={chordConts} />
</Shell>

<style>
  :global(html), :global(body), :global(#app) { height: 100%; }
  :global(body) { overflow: hidden; -webkit-font-smoothing: antialiased; -moz-osx-font-smoothing: grayscale; }
  :global(*) { scrollbar-width: thin; scrollbar-color: var(--border) transparent; }
  :global(::-webkit-scrollbar) { width: 10px; height: 10px; }
  :global(::-webkit-scrollbar-track) { background: transparent; }
  :global(::-webkit-scrollbar-thumb) { border: 3px solid transparent; border-radius: 999px; background: transparent; background-clip: padding-box; }
  :global(*:hover::-webkit-scrollbar-thumb) { background-color: var(--border); background-clip: padding-box; }
  .errors { position: absolute; top: 0; right: 0; left: 0; z-index: 60; }
  .error { display: block; width: 100%; padding: 8px 16px; border: 0; border-bottom: 1px solid color-mix(in oklab, var(--danger) 30%, transparent); background: color-mix(in oklab, var(--danger) 20%, var(--surface)); color: var(--danger); font-size: 14px; text-align: left; }
  button.error { cursor: pointer; }
  .notice { display: block; width: 100%; padding: 8px 16px; border: 0; border-bottom: 1px solid color-mix(in oklab, var(--accent) 30%, transparent); background: color-mix(in oklab, var(--accent) 18%, var(--surface)); color: var(--accent); font-size: 14px; text-align: left; cursor: pointer; }
</style>

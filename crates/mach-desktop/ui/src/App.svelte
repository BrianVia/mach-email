<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import { Keymap, keyEventToChord, type KeyContext, type Mode } from "./lib/keymap";
  import { threadToMarkdown } from "./lib/markdown";
  import { splitOf, type Split } from "./lib/split";
  import {
    dispatchAction,
    fetchAccountStatus,
    fetchKeymapSources,
    fetchOutboxSummary,
    fetchSettings,
    fetchSendLaterPresets,
    flushOutbox,
    listLabels,
    listScheduled,
    loadOlder,
    listThreads,
    openDraft,
    openThread as openThreadIpc,
    refetchThread as refetchThreadIpc,
    retryOutbox,
    searchThreads,
    syncNow,
    unsubscribePost,
    unsubscribeMailto,
    type AccountStatus,
    type ActionOutcome,
    type Draft,
    type Label,
    type Message,
    type MailSyncedPayload,
    type OutboxSummary,
    type Settings,
    type ScheduledSend,
    type SyncStatusPayload,
    type ThreadSummary,
    type UnsubscribeTarget,
  } from "./lib/ipc";
  import Shell from "./Shell.svelte";
  import Inbox from "./views/Inbox.svelte";
  import ThreadReader from "./views/Thread.svelte";
  import Composer from "./views/Composer.svelte";
  import SearchOverlay from "./views/Search.svelte";
  import Palette from "./views/Palette.svelte";
  import ChordOverlay from "./views/ChordOverlay.svelte";
  import Scheduled from "./views/Scheduled.svelte";

  const INITIAL_LIST_LIMIT = 1000;

  type InboxView = { kind: "inbox"; label: string; threads: ThreadSummary[]; selected: number; limit: number };
  type ThreadOrigin = { threads: ThreadSummary[]; index: number };
  type ThreadView = { kind: "thread"; thread: ThreadSummary; messages: Message[]; selectedMsg: number; origin?: ThreadOrigin };
  type ComposerFields = { to: string; cc: string; bcc: string; subject: string; body_md: string };
  type ComposerView = { kind: "composer"; draft: Draft; background: AppView };
  type ScheduledView = { kind: "scheduled"; sends: ScheduledSend[]; selected: number };
  type SearchView = { kind: "search"; query: string; results: ThreadSummary[]; selected: number; background: AppView };
  type PaletteView = { kind: "palette"; query: string; selected: number; background: AppView };
  type AppView = InboxView | ThreadView | ComposerView | ScheduledView | SearchView | PaletteView;
  type PaletteCommand = { label: string; chord: string };
  type Continuation = { next: string; action_name: string };

  let view = $state<AppView>({ kind: "inbox", label: "INBOX", threads: [], selected: 0, limit: INITIAL_LIST_LIMIT });
  let inboxSplit = $state<Split>("important");
  let keymap = $state<Keymap | null>(null);
  let bootError = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let unsubscribeConfirm = $state<{ sender: string; accountId: string; target: UnsubscribeTarget } | null>(null);
  let chordBuf = $state("");
  let chordConts = $state<Continuation[]>([]);
  let status = $state<AccountStatus | null>(null);
  let outbox = $state<OutboxSummary>({ pending: 0, failed: 0, last_error: null });
  let settings = $state<Settings>({});
  let labels = $state<Label[]>([]);
  let syncOkByAccount = $state<Record<string, boolean>>({});
  let composerFields: ComposerFields = { to: "", cc: "", bcc: "", subject: "", body_md: "" };
  let sendLaterOptions = $state<[string, string][]>([]);

  function composerTitle(draft: Draft) {
    if (draft.in_reply_to_message_id) return "Reply";
    if (draft.subject.startsWith("Fwd:")) return "Forward";
    return "New message";
  }
  let actionErrorTimer: number | undefined;
  let noticeTimer: number | undefined;

  let title = $derived.by(() => {
    if (view.kind === "inbox") return labelDisplay(view.label);
    if (view.kind === "thread") return view.thread.subject || "(no subject)";
    if (view.kind === "composer") return composerTitle(view.draft);
    if (view.kind === "scheduled") return "Scheduled";
    return "Search";
  });

  let subtitle = $derived.by(() => {
    if (view.kind === "inbox") return `${visibleInboxThreads(view).length.toLocaleString()} threads`;
    if (view.kind === "thread") {
      return `${view.thread.participants.slice(0, 2).join(", ")}${view.thread.participants.length > 2 ? ` +${view.thread.participants.length - 2}` : ""}`;
    }
    if (view.kind === "scheduled") return `${view.sends.length.toLocaleString()} messages`;
    return status?.email ?? "";
  });

  let activeLabel = $derived(view.kind === "inbox" ? view.label : view.kind === "scheduled" ? "SCHEDULED" : undefined);
  let allAccountsSynced = $derived(
    (status?.accounts.length ?? 0) > 0
      && (status?.accounts.every((account) => syncOkByAccount[account] === true) ?? false),
  );
  let userLabels = $derived.by(() => labels
    .filter((label) => !label.system && !label.name.startsWith("MACH/"))
    .sort((a, b) => a.name.localeCompare(b.name)));

  function showActionError(error: unknown) {
    actionError = String((error as Error).message ?? error);
    if (actionErrorTimer !== undefined) window.clearTimeout(actionErrorTimer);
    actionErrorTimer = window.setTimeout(() => (actionError = null), 4_000);
  }

  function visibleInboxThreads(inbox: InboxView): ThreadSummary[] {
    return inbox.label === "INBOX"
      ? inbox.threads.filter((thread) => splitOf(thread.label_ids) === inboxSplit)
      : inbox.threads;
  }

  function selectInboxSplit(split: Split) {
    inboxSplit = split;
    localStorage.setItem("mach.inboxSplit", split);
    if (view.kind === "inbox" && view.label === "INBOX") {
      view = { ...view, selected: clamp(view.selected, 0, visibleInboxThreads(view).length - 1) };
    }
  }

  function showNotice(message: string | null) {
    notice = message;
    if (noticeTimer !== undefined) window.clearTimeout(noticeTimer);
    if (message) noticeTimer = window.setTimeout(() => (notice = null), 4_000);
  }

  function copyThreadAsMarkdown(threadView: ThreadView) {
    void navigator.clipboard.writeText(threadToMarkdown(threadView.thread, threadView.messages))
      .then(() => showNotice("Copied thread as Markdown"))
      .catch((error) => console.warn("[mach] copying thread as Markdown failed", error));
  }

  async function boot() {
    try {
      const savedSplit = localStorage.getItem("mach.inboxSplit");
      if (savedSplit === "important" || savedSplit === "other" || savedSplit === "newsletters") {
        inboxSplit = savedSplit;
      }
      try {
        settings = await fetchSettings();
      } catch (error) {
        console.warn("[mach] settings load failed", error);
        settings = {};
      }
      labels = await listLabels();
      const sources = await fetchKeymapSources();
      try {
        const resolved = Keymap.fromToml(sources.defaults);
        if (sources.user) resolved.merge(Keymap.fromToml(sources.user));
        keymap = resolved;
      } catch (error) {
        console.error("[mach] keymap parse failed:", error);
        bootError = `keymap parse: ${(error as Error).message ?? error}`;
      }
      const threads = await listThreads("INBOX", INITIAL_LIST_LIMIT);
      console.log(`[mach] loaded ${threads.length} threads`);
      view = { kind: "inbox", label: "INBOX", threads, selected: 0, limit: INITIAL_LIST_LIMIT };
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

  async function refreshOutboxSummary() {
    try {
      outbox = await fetchOutboxSummary();
    } catch (error) {
      console.warn("[mach] outbox summary failed", error);
    }
  }

  async function refreshInboxPreservingSelection() {
    const currentView = view;
    if (currentView.kind !== "inbox") return;
    const selectedId = visibleInboxThreads(currentView)[currentView.selected]?.id;
    const threads = await listThreads(currentView.label, currentView.limit);
    if (view.kind !== "inbox" || view.label !== currentView.label) return;
    const visible = visibleInboxThreads({ ...currentView, threads });
    const preserved = selectedId
      ? visible.findIndex((thread) => thread.id === selectedId)
      : -1;
    view = {
      ...view,
      threads,
      selected: preserved >= 0
        ? preserved
        : clamp(view.selected, 0, visible.length - 1),
    };
  }

  function handleMailSynced(_payload: MailSyncedPayload) {
    void listLabels().then((next) => (labels = next)).catch((error) => {
      console.warn("[mach] refreshing labels after sync failed", error);
    });
    if (view.kind === "inbox") {
      void refreshInboxPreservingSelection().catch((error) => {
        console.warn("[mach] refreshing inbox after sync failed", error);
      });
    }
    if (view.kind === "scheduled") {
      void listScheduled().then((sends) => {
        if (view.kind === "scheduled") view = { ...view, sends, selected: clamp(view.selected, 0, sends.length - 1) };
      }).catch((error) => console.warn("[mach] refreshing scheduled sends failed", error));
    }
  }

  function handleSyncStatus(payload: SyncStatusPayload) {
    syncOkByAccount = { ...syncOkByAccount, [payload.account]: payload.ok };
    if (!payload.ok) void refreshStatus();
    void refreshOutboxSummary();
  }

  function handleKey(event: KeyboardEvent) {
    if (!keymap) {
      console.warn("[mach] keymap not loaded yet; key dropped", event.key);
      return;
    }

    const currentView = view;
    if (currentView.kind === "scheduled" && (event.key === "Enter" || event.key === "#")) {
      event.preventDefault();
      if (event.key === "Enter") void openScheduledRow(currentView.selected);
      else void cancelScheduledRow(currentView.selected);
      return;
    }
    const chord = keyEventToChord(event);
    if (!chord) return;
    const openThread = currentView.kind === "thread"
      ? currentView
      : currentView.kind === "palette" && currentView.background.kind === "thread"
        ? currentView.background
        : null;
    if (chord === "ctrl+shift+c" && openThread) {
      event.preventDefault();
      copyThreadAsMarkdown(openThread);
      return;
    }
    if (chord === "ctrl+k") {
      if (currentView.kind === "palette") {
        event.preventDefault();
        view = currentView.background;
        return;
      }
      if (currentView.kind === "inbox" || currentView.kind === "thread") {
        event.preventDefault();
        chordBuf = "";
        chordConts = [];
        view = { kind: "palette", query: "", selected: 0, background: currentView };
        return;
      }
    }
    if (currentView.kind === "palette" && chord === "esc") {
      event.preventDefault();
      view = currentView.background;
      return;
    }

    const target = event.target as HTMLElement | null;
    const inTextInput = target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);
    if (inTextInput && !isControlKey(event)) return;
    if ((currentView.kind === "composer" || currentView.kind === "search" || currentView.kind === "palette") && !isControlKey(event)) return;

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
      const thread = visibleInboxThreads(currentView)[currentView.selected];
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
    if (currentView.kind === "palette") return currentContext(currentView.background);
    if (currentView.kind === "scheduled") return { selection: [] };
    return { selection: [], current_draft: currentView.draft.id };
  }

  function currentMode(currentView: AppView): Mode {
    switch (currentView.kind) {
      case "inbox": return "normal";
      case "thread": return "reading";
      case "composer": return "composing";
      case "search": return "search";
      case "palette": return "normal";
      case "scheduled": return "normal";
    }
  }

  function paletteCommands(background: AppView): PaletteCommand[] {
    if (!keymap) return [];
    const mode = currentMode(background);
    const context = currentContext(background);
    const excluded = new Set(["select_next", "select_prev", "quit"]);
    const labels: Record<string, string> = {
      archive: "Archive",
      trash: "Move to Trash",
      forward: "Forward",
      compose_new: "Compose",
      search: "Search",
      open_thread: "Open thread",
      back_to_list: "Back to list",
      undo: "Undo",
      redo: "Redo",
      refresh: "Refresh",
      mute: "Mute thread",
      unsubscribe: "Unsubscribe",
    };
    const byLabel = new Map<string, PaletteCommand>();

    for (const binding of keymap.bindings(mode)) {
      if (excluded.has(binding.name)) continue;
      let label = labels[binding.name];
      if (binding.name === "star") label = binding.chord === "shift+s" ? "Unstar" : "Star";
      else if (binding.name === "mark_read") label = binding.chord === "shift+u" ? "Mark read" : "Mark unread";
      else if (binding.name === "reply") label = binding.chord === "shift+r" ? "Reply all" : "Reply";
      else if (binding.name === "open_label") {
        const resolution = keymap.resolve(mode, binding.chord, context);
        const labelId = resolution.kind === "action" ? resolution.action.label_id as string | undefined : undefined;
        label = labelId ? `Go to ${titleCase(labelId)}` : "Open label";
      }
      label ??= titleCase(binding.name);

      const command = { label, chord: binding.chord };
      const existing = byLabel.get(label);
      if (!existing || chordLength(command.chord) < chordLength(existing.chord)) byLabel.set(label, command);
    }
    const commands = [...byLabel.values()];
    if (background.kind === "thread") commands.push({ label: "Copy as Markdown", chord: "ctrl+shift+c" });
    commands.push({ label: "Retry failed changes", chord: "retry" });
    return commands;
  }

  function filteredPaletteCommands(currentView: PaletteView) {
    const query = currentView.query.trim().toLocaleLowerCase();
    return paletteCommands(currentView.background).filter((command) =>
      command.label.toLocaleLowerCase().includes(query),
    );
  }

  function onPaletteInput(query: string) {
    if (view.kind === "palette") view = { ...view, query, selected: 0 };
  }

  function movePaletteSelection(delta: number) {
    if (view.kind !== "palette") return;
    const commands = filteredPaletteCommands(view);
    view = { ...view, selected: clamp(view.selected + delta, 0, commands.length - 1) };
  }

  function runPaletteCommand(index = view.kind === "palette" ? view.selected : 0) {
    const currentView = view;
    if (currentView.kind !== "palette" || !keymap) return;
    const background = currentView.background;
    const command = filteredPaletteCommands(currentView)[index];
    if (!command) return;
    view = background;
    if (command.label === "Copy as Markdown" && background.kind === "thread") {
      copyThreadAsMarkdown(background);
      return;
    }
    if (command.label === "Retry failed changes") {
      void retryFailedChanges();
      return;
    }
    const resolution = keymap.resolve(currentMode(background), command.chord, currentContext(background));
    if (resolution.kind === "action") void runAction(resolution.action);
  }

  async function retryFailedChanges() {
    try {
      const retried = await retryOutbox();
      const report = await flushOutbox();
      await refreshOutboxSummary();
      if (report.failed > 0) showActionError(report.last_error ?? "Outbox retry failed");
      else showNotice(`${retried} failed change(s) retried`);
    } catch (error) {
      showActionError(error);
    }
  }

  function chordLength(chord: string) {
    return chord.trim().split(/\s+/).length;
  }

  function titleCase(value: string) {
    return value.toLocaleLowerCase().split("_").map((word) =>
      word ? word[0].toLocaleUpperCase() + word.slice(1) : word,
    ).join(" ");
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
          const threads = await listThreads("INBOX", INITIAL_LIST_LIMIT);
          let selected = 0;
          if (currentView.kind === "thread" && currentView.origin) {
            const { origin } = currentView;
            const selectedId = origin.threads[origin.index]?.id;
            const visible = visibleInboxThreads({ kind: "inbox", label: "INBOX", threads, selected: 0, limit: INITIAL_LIST_LIMIT });
            const preserved = selectedId
              ? visible.findIndex((thread) => thread.id === selectedId)
              : -1;
            selected = preserved >= 0
              ? preserved
              : clamp(origin.index, 0, visible.length - 1);
          }
          view = { kind: "inbox", label: "INBOX", threads, selected, limit: INITIAL_LIST_LIMIT };
          return;
        }
        case "select_next":
          moveSelection(1);
          return;
        case "select_prev":
          moveSelection(-1);
          return;
        case "inbox_split_important":
        case "inbox_split_other":
        case "inbox_split_newsletters":
          if (currentView.kind === "inbox" && currentView.label === "INBOX") {
            selectInboxSplit(kind.slice("inbox_split_".length) as Split);
          }
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
        case "send_later": {
          if (currentView.kind !== "composer") return;
          const draft = await saveComposerDraft(currentView);
          const at = action.at as string;
          await dispatchAction({ kind: "send_later", draft_id: draft.id, at });
          view = currentView.background;
          showNotice(`Scheduled for ${new Date(at).toLocaleString([], { dateStyle: "medium", timeStyle: "short" })}`);
          return;
        }
        case "unsubscribe": {
          await beginUnsubscribe(action.message_id as string);
          return;
        }
        case "open_thread": {
          const id = (action.id as string) ?? "";
          const thread = currentView.kind === "inbox"
            ? visibleInboxThreads(currentView).find((candidate) => candidate.id === id)
            : currentView.kind === "search"
              ? currentView.results.find((candidate) => candidate.id === id)
              : undefined;
          if (!thread) throw new Error("thread not found");
          const at = currentView.kind === "inbox"
            ? visibleInboxThreads(currentView).findIndex((candidate) => candidate.id === id)
            : -1;
          await openThreadView(
            thread,
            currentView.kind === "inbox" ? { threads: visibleInboxThreads(currentView), index: at } : undefined,
          );
          return;
        }
        case "open_label": {
          const label = action.label_id as string;
          if (label === "SCHEDULED") {
            view = { kind: "scheduled", sends: await listScheduled(), selected: 0 };
            return;
          }
          const threads = await listThreads(label, INITIAL_LIST_LIMIT);
          view = { kind: "inbox", label, threads, selected: 0, limit: INITIAL_LIST_LIMIT };
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
            await syncNow();
            await refreshInboxPreservingSelection();
          }
          return;
        }
      }

      const isArchiveOrTrash = kind === "archive" || kind === "mute" || kind === "trash";
      if (
        isArchiveOrTrash
        && currentView.kind === "thread"
        && currentView.origin
        && (settings.after_archive ?? "next") === "next"
      ) {
        const { threads, index } = currentView.origin;
        const archivedId = currentView.thread.id;
        const remaining = threads.filter((thread) => thread.id !== archivedId);
        void dispatchAction(action).catch(showActionError);
        const nextIndex = Math.min(index, remaining.length - 1);
        const next = remaining[nextIndex];
        if (next) {
          await openThreadView(next, { threads: remaining, index: nextIndex });
        } else {
          const fresh = await listThreads("INBOX", INITIAL_LIST_LIMIT);
          view = { kind: "inbox", label: "INBOX", threads: fresh, selected: 0, limit: INITIAL_LIST_LIMIT };
        }
        return;
      }

      // List-view archive/trash: prune the list from memory and dispatch in
      // the background — the optimistic local write + outbox handle the rest.
      if (isArchiveOrTrash && currentView.kind === "inbox") {
        const removed = new Set((action.thread_ids as string[] | undefined) ?? []);
        if (removed.size) {
          void dispatchAction(action).catch(showActionError);
          const threads = currentView.threads.filter((thread) => !removed.has(thread.id));
          const visible = visibleInboxThreads({ ...currentView, threads });
          view = { ...currentView, threads, selected: Math.min(currentView.selected, Math.max(0, visible.length - 1)) };
          return;
        }
      }

      const outcome = await dispatchAction(action);
      const removedSet = new Set(outcome.changed_threads);

      if (isArchiveOrTrash && currentView.kind === "inbox" && removedSet.size) {
        const threads = currentView.threads.filter((thread) => !removedSet.has(thread.id));
        const visible = visibleInboxThreads({ ...currentView, threads });
        view = { ...currentView, threads, selected: Math.min(currentView.selected, Math.max(0, visible.length - 1)) };
      }

      if (isArchiveOrTrash && currentView.kind === "thread") {
        const threads = await listThreads("INBOX", INITIAL_LIST_LIMIT);
        const visible = visibleInboxThreads({ kind: "inbox", label: "INBOX", threads, selected: 0, limit: INITIAL_LIST_LIMIT });
        const at = visible.findIndex((thread) => thread.id === currentView.thread.id);
        // The archived thread is usually gone from the refreshed list; the
        // thread now occupying its date-sorted position is the "next" one.
        const successor = visible.findIndex(
          (thread) => thread.last_message_at <= currentView.thread.last_message_at,
        );
        const fallback = at >= 0
          ? Math.min(at, visible.length - 1)
          : successor >= 0 ? successor : Math.max(0, visible.length - 1);
        view = { kind: "inbox", label: "INBOX", threads, selected: fallback, limit: INITIAL_LIST_LIMIT };
      }

      if (!isArchiveOrTrash && currentView.kind === "inbox" && removedSet.size) {
        const selectedId = visibleInboxThreads(currentView)[currentView.selected]?.id;
        const threads = await listThreads(currentView.label, currentView.limit);
        const visible = visibleInboxThreads({ ...currentView, threads });
        const preserved = selectedId ? visible.findIndex((thread) => thread.id === selectedId) : -1;
        view = {
          ...currentView,
          threads,
          selected: preserved >= 0 ? preserved : clamp(currentView.selected, 0, visible.length - 1),
        };
      }
    } catch (error) {
      console.warn("dispatch failed", error);
      if (kind === "send_draft") showNotice(null);
      showActionError(error);
    }
  }

  async function openThreadView(thread: ThreadSummary, origin?: ThreadOrigin) {
    const cached = await openThreadIpc(thread.id, false);
    view = { kind: "thread", thread: cached.thread, messages: cached.messages, selectedMsg: 0, origin };
    prefetchThread(origin?.threads[origin.index + 1]?.id);
    if (thread.unread) {
      void dispatchAction({ kind: "mark_read", thread_ids: [thread.id], read: true }).catch(() => {});
    }
    if (cached.messages.some((message) => !message.fetched_full)) {
      void openThreadIpc(thread.id, true).then((result) => {
        if (view.kind !== "thread" || view.thread.id !== thread.id) return;
        view = { ...view, thread: result.thread, messages: result.messages };
        if (result.body_fetch_error) {
          showActionError(new Error(`couldn't fetch full message — showing preview (${result.body_fetch_error})`));
        }
      }).catch((error) => {
        if (view.kind !== "thread" || view.thread.id !== thread.id) return;
        showActionError(error);
      });
    }
  }

  async function beginUnsubscribe(messageId: string) {
    const currentView = view;
    const message = currentView.kind === "thread"
      ? currentView.messages.find((candidate) => candidate.id === messageId)
      : undefined;
    const outcome = await dispatchAction({ kind: "unsubscribe", message_id: messageId });
    const targets = (outcome.data as { targets?: UnsubscribeTarget[] } | null)?.targets ?? [];
    if (!targets[0]) {
      showNotice("No unsubscribe method advertised");
      return;
    }
    if (!message) throw new Error("unsubscribe message not found");
    unsubscribeConfirm = { sender: message.from, accountId: message.account_id, target: targets[0] };
  }

  async function confirmUnsubscribe() {
    const confirmation = unsubscribeConfirm;
    unsubscribeConfirm = null;
    if (!confirmation) return;
    const target = confirmation.target;
    if (target.kind === "https") {
      if (target.one_click) await unsubscribePost(target.url);
      else await openUrl(target.url);
    } else {
      await unsubscribeMailto(confirmation.accountId, target.to, target.subject);
    }
    showNotice("Unsubscribed");
  }

  function prefetchThread(threadId: string | undefined) {
    if (!threadId) return;
    void openThreadIpc(threadId, true).catch(() => {});
  }

  function openComposer(draft: Draft, background: AppView) {
    composerFields = fieldsFromDraft(draft);
    void fetchSendLaterPresets().then((presets) => (sendLaterOptions = presets)).catch(showActionError);
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
      bcc: draft.bcc.join(", "),
      subject: draft.subject,
      body_md: draft.body_md,
    };
  }

  function draftPatch() {
    return {
      to: parseRecipients(composerFields.to),
      cc: parseRecipients(composerFields.cc),
      bcc: parseRecipients(composerFields.bcc),
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
      view = { ...currentView, selected: clamp(currentView.selected + delta, 0, visibleInboxThreads(currentView).length - 1) };
    } else if (currentView.kind === "thread") {
      view = { ...currentView, selectedMsg: clamp(currentView.selectedMsg + delta, 0, currentView.messages.length - 1) };
    } else if (currentView.kind === "search") {
      view = { ...currentView, selected: clamp(currentView.selected + delta, 0, currentView.results.length - 1) };
    } else if (currentView.kind === "scheduled") {
      view = { ...currentView, selected: clamp(currentView.selected + delta, 0, currentView.sends.length - 1) };
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
    const threads = visibleInboxThreads(view);
    const thread = threads[index];
    if (!thread) return;
    try {
      await openThreadView(thread, { threads, index });
    } catch (error) {
      console.warn("open thread failed", error);
      showActionError(error);
    }
  }

  async function openScheduledRow(index: number) {
    const currentView = view;
    if (currentView.kind !== "scheduled") return;
    const send = currentView.sends[index];
    if (!send) return;
    try {
      openComposer(await openDraft(send.draft_id), currentView);
    } catch (error) {
      showActionError(error);
    }
  }

  async function cancelScheduledRow(index: number) {
    const currentView = view;
    if (currentView.kind !== "scheduled") return;
    const send = currentView.sends[index];
    if (!send) return;
    try {
      await dispatchAction({ kind: "cancel_send_later", send_later_id: send.send_later_id });
      const sends = currentView.sends.filter((candidate) => candidate.send_later_id !== send.send_later_id);
      view = { ...currentView, sends, selected: clamp(currentView.selected, 0, sends.length - 1) };
      showNotice("Scheduled send cancelled");
    } catch (error) {
      showActionError(error);
    }
  }

  async function loadOlderMail(): Promise<number | null> {
    const currentView = view;
    if (currentView.kind !== "inbox") return null;
    const beforeMs = Date.parse(currentView.threads.at(-1)?.last_message_at ?? "");
    if (!Number.isFinite(beforeMs)) return 0;
    try {
      const stats = await loadOlder(currentView.label, beforeMs);
      const limit = currentView.limit + 200;
      const threads = await listThreads(currentView.label, limit);
      if (view.kind === "inbox" && view.label === currentView.label) {
        view = { ...view, threads, limit };
      }
      return stats.fetched;
    } catch (error) {
      showActionError(error);
      return null;
    }
  }

  onMount(() => {
    void boot();
    void refreshStatus();
    void refreshOutboxSummary();
    let destroyed = false;
    const unlisteners: Array<() => void> = [];
    const keepUnlistener = (unlisten: () => void) => {
      if (destroyed) unlisten();
      else unlisteners.push(unlisten);
    };
    void listen<MailSyncedPayload>("mail-synced", (event) => {
      handleMailSynced(event.payload);
    }).then(keepUnlistener).catch((error) => {
      console.warn("[mach] mail-synced listener failed", error);
    });
    void listen<SyncStatusPayload>("sync-status", (event) => {
      handleSyncStatus(event.payload);
    }).then(keepUnlistener).catch((error) => {
      console.warn("[mach] sync-status listener failed", error);
    });
    document.addEventListener("keydown", handleKey, true);
    window.focus();
    document.body.tabIndex = -1;
    document.body.focus();

    return () => {
      destroyed = true;
      for (const unlisten of unlisteners) unlisten();
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
      INBOX: "Inbox", STARRED: "Starred", SENT: "Sent", DRAFT: "Drafts", SCHEDULED: "Scheduled", TRASH: "Trash", SPAM: "Spam", DONE: "Done", SNOOZED: "Snoozed", MUTED: "Muted", ALL: "All Mail",
    };
    return names[id] ?? userLabels.find((label) => label.id === id)?.name ?? id;
  }
</script>

<Shell
  {title}
  {subtitle}
  accountEmail={status?.email}
  online={allAccountsSynced}
  {outbox}
  {activeLabel}
  {chordBuf}
  {chordConts}
  {userLabels}
  onOpenLabel={(label) => void runAction({ kind: "open_label", label_id: label })}
>
  {#if view.kind === "inbox"}
    <Inbox
      v={view}
      split={inboxSplit}
      onSplit={selectInboxSplit}
      onSelect={(selected) => {
        if (view.kind === "inbox") view = { ...view, selected };
      }}
      onOpen={(index) => void openInboxRow(index)}
      onLoadOlder={loadOlderMail}
    />
  {:else if view.kind === "scheduled"}
    <Scheduled
      sends={view.sends}
      selected={view.selected}
      onSelect={(selected) => { if (view.kind === "scheduled") view = { ...view, selected }; }}
      onOpen={(index) => void openScheduledRow(index)}
      onCancel={(index) => void cancelScheduledRow(index)}
    />
  {:else if view.kind === "thread"}
    <ThreadReader v={view} onUnsubscribe={(messageId) => void beginUnsubscribe(messageId).catch(showActionError)} />
  {:else if view.kind === "composer"}
    <Composer
      draft={view.draft}
      title={composerTitle(view.draft)}
      onFieldsChange={(fields) => (composerFields = fields)}
      onSend={() => void runAction({ kind: "send_draft", draft_id: view.kind === "composer" ? view.draft.id : "" })}
      presets={sendLaterOptions}
      onSchedule={(at) => void runAction({ kind: "send_later", at })}
      onClose={closeComposer}
    />
  {:else if view.kind === "search"}
    <SearchOverlay
      v={view}
      onInput={onSearchInput}
      onEnter={onSearchEnter}
      onEsc={onSearchEsc}
      onMove={moveSelection}
      onPick={(index) => {
        if (view.kind !== "search") return;
        view = { ...view, selected: index };
        void onSearchEnter();
      }}
    />
  {:else if view.kind === "palette"}
    <Palette
      v={view}
      commands={paletteCommands(view.background)}
      onInput={onPaletteInput}
      onMove={movePaletteSelection}
      onEnter={() => runPaletteCommand()}
      onEsc={() => {
        if (view.kind === "palette") view = view.background;
      }}
      onPick={runPaletteCommand}
    />
  {/if}

  {#if bootError || actionError || notice || unsubscribeConfirm || status?.needs_reauth.length}
    <div class="errors">
      {#each status?.needs_reauth ?? [] as email}
        <div class="notice reauth">Sign-in expired for {email}. Run mach auth login in a terminal.</div>
      {/each}
      {#if bootError}<div class="error">⚠ {bootError}</div>{/if}
      {#if actionError}<button class="error" onclick={() => (actionError = null)}>⚠ {actionError}</button>{/if}
      {#if notice}<button class="notice" onclick={() => showNotice(null)}>{notice}</button>{/if}
      {#if unsubscribeConfirm}
        <div class="notice confirm">
          <span>Unsubscribe from {unsubscribeConfirm.sender}?</span>
          <button onclick={() => void confirmUnsubscribe().catch(showActionError)}>Unsubscribe</button>
          <button onclick={() => (unsubscribeConfirm = null)}>Cancel</button>
        </div>
      {/if}
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
  .reauth { cursor: default; }
  .confirm { display: flex; align-items: center; gap: 12px; cursor: default; }
  .confirm button { padding: 3px 8px; border: 1px solid var(--border); border-radius: 5px; background: var(--surface); color: inherit; cursor: pointer; }
</style>

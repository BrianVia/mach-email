// Wrappers around Tauri's `invoke` so the rest of the app stays cleanly
// typed and the JSON shape is documented once.

import { invoke as tauriInvoke } from "@tauri-apps/api/core";

// In Tauri, `invoke` is available. When running the Vite dev server in a
// plain browser tab (for frontend-only debugging), `invoke` is undefined.
// Wrap it so calls don't blow up the whole boot — they just resolve to
// `null` and the UI shows empty state.
function isInTauri(): boolean {
  return typeof tauriInvoke === "function" && "__TAURI_INTERNALS__" in window;
}
const invoke: typeof tauriInvoke = async (cmd, args) => {
  if (!isInTauri()) {
    console.warn(
      `[mach] invoke('${cmd}') called outside Tauri — returning null. Run via mach-desktop binary for real data.`,
    );
    return null as never;
  }
  return tauriInvoke(cmd, args);
};

export type ThreadSummary = {
  account_id: string;
  id: string;
  subject: string;
  snippet: string;
  participants: string[];
  last_message_at: string;
  message_count: number;
  unread: boolean;
  starred: boolean;
  label_ids: string[];
};

export type InlineImageRef = {
  content_id: string;
  attachment_id: string;
  mime_type: string;
  filename: string | null;
  size: number | null;
};

export type Message = {
  account_id: string;
  id: string;
  thread_id: string;
  from: string;
  to: string[];
  cc: string[];
  subject: string;
  snippet: string;
  internal_date: string;
  body_plain: string | null;
  body_html: string | null;
  label_ids: string[];
  fetched_full: boolean;
  inline_images: InlineImageRef[];
};

export type Draft = {
  account_id: string;
  id: string;
  thread_id: string | null;
  in_reply_to_message_id: string | null;
  to: string[];
  cc: string[];
  bcc: string[];
  subject: string;
  body_md: string;
  updated_at: string;
};

export type ActionOutcome = {
  action_name: string;
  op_id: string | null;
  changed_threads: string[];
  changed_drafts: string[];
  data: unknown;
  message: string;
};

export type AccountStatus = {
  email: string;
  accounts: string[];
  online: boolean;
};

export type MailSyncedPayload = {
  account: string;
};

export type SyncStatusPayload = {
  account: string;
  ok: boolean;
  error: string | null;
};

export type SyncNowReport = {
  synced: number;
  failed: number;
  last_error: string | null;
};

export type KeymapSources = {
  defaults: string;
  user: string | null;
};

export type Settings = {
  after_archive?: "next" | "list";
};

// Backend Ok/Err discriminated union.
type Out<T> = { ok: T } | { err: string };
function unwrap<T>(o: Out<T>): T {
  if ("err" in o) throw new Error(o.err);
  return o.ok;
}

export async function dispatchAction(actionJson: object): Promise<ActionOutcome> {
  return invoke<ActionOutcome>("dispatch_action", {
    actionJson: JSON.stringify(actionJson),
  });
}

export async function listThreads(
  labelId: string,
  limit = 200,
): Promise<ThreadSummary[]> {
  const r = await invoke<Out<ThreadSummary[]> | null>("list_threads", {
    labelId,
    limit,
  });
  if (!r) return [];
  return unwrap(r);
}

export async function openThread(
  threadId: string,
  fetch = true,
): Promise<{ thread: ThreadSummary; messages: Message[]; body_fetch_error?: string | null }> {
  const r = await invoke<Out<{ thread: ThreadSummary; messages: Message[]; body_fetch_error?: string | null }>>(
    "open_thread",
    { threadId, fetch },
  );
  return unwrap(r);
}

/**
 * Force a fresh fetch: wipes the body cache for this thread and re-runs
 * the parser. Use when a thread looks wrong (cached bodies from an older
 * parser, missing inline images, etc.).
 */
export async function refetchThread(
  threadId: string,
): Promise<{ thread: ThreadSummary; messages: Message[] }> {
  const r = await invoke<Out<{ thread: ThreadSummary; messages: Message[] }>>(
    "refetch_thread",
    { threadId },
  );
  return unwrap(r);
}

export async function searchThreads(
  query: string,
  limit = 50,
): Promise<ThreadSummary[]> {
  const r = await invoke<Out<ThreadSummary[]>>("search", { query, limit });
  return unwrap(r);
}

export async function fetchKeymapSources(): Promise<KeymapSources> {
  return invoke<KeymapSources>("keymap_sources");
}

export async function fetchSettings(): Promise<Settings> {
  return invoke<Settings>("settings");
}

export async function fetchAccountStatus(): Promise<AccountStatus> {
  return invoke<AccountStatus>("account_status");
}

export async function flushOutbox(): Promise<{
  processed: number;
  failed: number;
  last_error: string | null;
}> {
  const result = await invoke<Out<{
    processed: number;
    failed: number;
    last_error: string | null;
  }>>("flush_outbox");
  return unwrap(result);
}

export async function syncNow(): Promise<SyncNowReport> {
  const result = await invoke<Out<SyncNowReport>>("sync_now");
  return unwrap(result);
}

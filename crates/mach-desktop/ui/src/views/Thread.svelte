<script lang="ts">
  import { openUrl } from "@tauri-apps/plugin-opener";
  import type { Message, ThreadSummary } from "../lib/ipc";
  import { saveAttachment } from "../lib/ipc";
  import { linkify } from "../lib/text";
  import { renderEmailHtml } from "../lib/html";
  import { avatarColor, initialsFor } from "../lib/avatar";

  let { v, onUnsubscribe, onAttachmentSaved, onError }: {
    v: { kind: "thread"; thread: ThreadSummary; messages: Message[]; selectedMsg: number };
    onUnsubscribe: (messageId: string) => void;
    onAttachmentSaved: (path: string) => void;
    onError: (error: unknown) => void;
  } = $props();
  let overrides = $state<Record<string, boolean>>({});
  let shownRemoteImages = $state<Record<string, boolean>>({});
  let remoteImageAllow = $state(loadRemoteImageAllow());
  let previousSelected = $state<number | null>(null);

  function loadRemoteImageAllow(): string[] {
    try {
      const value: unknown = JSON.parse(localStorage.getItem("mach.remoteImageAllow") ?? "[]");
      return Array.isArray(value) ? value.filter((email): email is string => typeof email === "string").map((email) => email.toLowerCase()) : [];
    } catch {
      return [];
    }
  }

  function alwaysShowRemoteImages(email: string) {
    remoteImageAllow = [...new Set([...remoteImageAllow, email.toLowerCase()])];
    localStorage.setItem("mach.remoteImageAllow", JSON.stringify(remoteImageAllow));
  }

  $effect(() => {
    const selected = v.selectedMsg;
    if (previousSelected !== null && selected !== previousSelected) {
      overrides = {};
    }
    previousSelected = selected;
  });

  function prettyFullDate(iso: string): string {
    const date = new Date(iso);
    const day = 24 * 60 * 60 * 1000;
    if (Date.now() - date.getTime() < day) {
      return date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
    }
    return date.toLocaleDateString([], { weekday: "short", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
  }

  function senderName(from: string): string {
    return from.match(/^\s*"?([^"<]+?)"?\s*<.+>$/)?.[1].trim() ?? from;
  }

  function senderEmail(from: string): string {
    return (from.match(/<([^>]+)>/)?.[1] ?? from.match(/[^\s<>@]+@[^\s<>@]+/)?.[0] ?? "").trim();
  }

  function humanSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  }

  // The webview has no handler for target="_blank", so anchor clicks
  // silently die. Route them to the OS default browser instead.
  function interceptLinks(node: HTMLElement) {
    function onClick(event: MouseEvent) {
      const anchor = (event.target as HTMLElement | null)?.closest("a");
      if (!anchor) return;
      const href = anchor.getAttribute("href") ?? "";
      if (!/^(https?:|mailto:)/i.test(href)) return;
      event.preventDefault();
      void openUrl(href).catch((error) => console.warn("[mach] open link failed", error));
    }
    node.addEventListener("click", onClick);
    return { destroy: () => node.removeEventListener("click", onClick) };
  }
</script>

<div class="reader" use:interceptLinks>
  <div class="column">
    <header class="thread-header">
      <h1>{v.thread.subject || "(no subject)"}</h1>
      <p>{v.messages.length} {v.messages.length === 1 ? "message" : "messages"} · {v.thread.participants.length} {v.thread.participants.length === 1 ? "participant" : "participants"}</p>
    </header>
    <div class="cards">
      {#each v.messages as message, index (message.id)}
        {@const selected = index === v.selectedMsg}
        {@const expanded = overrides[message.id] ?? selected}
        <article class:selected>
          <header
            class="message-header"
            role="button"
            tabindex="0"
            onclick={() => (overrides[message.id] = !expanded)}
            onkeydown={(event) => {
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                overrides[message.id] = !expanded;
              }
            }}
          >
            <span class="avatar" style:background={avatarColor(message.from)} aria-hidden="true">{initialsFor(message.from)}</span>
            <div class="message-meta">
              <div class="sender">
                <span>{senderName(message.from)}</span>
                {#if senderEmail(message.from)}<small>{senderEmail(message.from)}</small>{/if}
                {#if selected && message.headers?.list_unsubscribe}
                  <button
                    class="unsubscribe"
                    type="button"
                    onclick={(event) => {
                      event.stopPropagation();
                      onUnsubscribe(message.id);
                    }}
                  >Unsubscribe</button>
                {/if}
              </div>
              <p>to {message.to.join(", ") || "—"}</p>
              {#if !expanded}<div class="preview">{(message.body_plain ?? message.snippet ?? "").slice(0, 200)}</div>{/if}
            </div>
            <time>{prettyFullDate(message.internal_date)}</time>
          </header>
          {#if expanded}
            {#if !message.fetched_full}
              <p class="preview-only">Preview only — full message not fetched yet. Press <kbd>⌃R</kbd> to retry.</p>
            {/if}
            {#if message.body_html && message.body_html.length > 0}
              {@const email = senderEmail(message.from).toLowerCase()}
              {@const rendered = renderEmailHtml(message, { showRemote: shownRemoteImages[message.id] || remoteImageAllow.includes(email) })}
              {#if rendered.blockedRemoteCount > 0}
                <div class="remote-images-bar">
                  <span>{rendered.blockedRemoteCount} remote {rendered.blockedRemoteCount === 1 ? "image" : "images"} blocked</span>
                  <span aria-hidden="true">·</span>
                  <button type="button" onclick={() => (shownRemoteImages[message.id] = true)}>Show images</button>
                  {#if email}
                    <span aria-hidden="true">·</span>
                    <button type="button" onclick={() => alwaysShowRemoteImages(email)}>Always show from {email}</button>
                  {/if}
                </div>
              {/if}
              <div class="message-body mach-html">{@html rendered.html}</div>
            {:else}
              <div class="message-body plain">{@html linkify(message.body_plain ?? message.snippet ?? "(no body)")}</div>
            {/if}
            {#if message.attachments?.length}
              <div class="attachments">
                {#each message.attachments as attachment (attachment.attachment_id)}
                  <button
                    type="button"
                    onclick={() => void saveAttachment(message.account_id, message.id, attachment.attachment_id, attachment.filename).then(onAttachmentSaved).catch(onError)}
                  >📎 {attachment.filename} <small>{humanSize(attachment.size)}</small></button>
                {/each}
              </div>
            {/if}
          {/if}
        </article>
      {/each}
    </div>
  </div>
</div>

<style>
  .reader { height: 100%; overflow-y: auto; }
  .column { max-width: 48rem; margin: 0 auto; padding: 32px; }
  .thread-header { margin-bottom: 32px; }
  h1 { margin: 0; color: var(--text); font-size: 22px; font-weight: 600; line-height: 1.2; letter-spacing: -.01em; }
  .thread-header p { margin: 8px 0 0; color: var(--muted); font-size: 12px; letter-spacing: .08em; text-transform: uppercase; }
  .cards { display: flex; flex-direction: column; gap: 12px; }
  article { overflow: hidden; border: 1px solid transparent; border-radius: 12px; background: var(--surface-2); transition: background 150ms; }
  article:not(.selected):hover { background: var(--hover); }
  article.selected { border-color: color-mix(in oklab, var(--accent) 30%, transparent); background: var(--surface); box-shadow: 0 5px 18px color-mix(in oklab, black 14%, transparent); }
  .message-header { display: flex; align-items: flex-start; gap: 12px; padding: 16px 20px; cursor: pointer; user-select: none; }
  .avatar { display: inline-flex; width: 32px; height: 32px; flex-shrink: 0; align-items: center; justify-content: center; border-radius: 50%; color: white; font-size: 12px; font-weight: 600; }
  .message-meta { min-width: 0; flex: 1; }
  .sender { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; }
  .sender span { overflow: hidden; color: var(--text); font-size: 14px; font-weight: 600; text-overflow: ellipsis; white-space: nowrap; }
  .sender small { overflow: hidden; color: var(--muted); font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
  .unsubscribe { padding: 0; border: 0; background: none; color: var(--accent); font-size: 11px; cursor: pointer; }
  .message-meta p { margin: 2px 0 0; color: var(--muted); font-size: 11.5px; }
  time { flex-shrink: 0; color: var(--muted); font-size: 11px; font-variant-numeric: tabular-nums; }
  .preview { display: -webkit-box; overflow: hidden; margin-top: 8px; color: var(--muted); font-size: 13px; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; }
  .message-body { padding: 0 20px 20px; overflow-wrap: anywhere; color: var(--muted); font-size: 14px; line-height: 1.6; }
  .attachments { display: flex; flex-wrap: wrap; gap: 8px; padding: 0 20px 20px; }
  .attachments button { padding: 6px 10px; border: 1px solid var(--border); border-radius: 999px; background: var(--surface-2); color: var(--text); font: inherit; font-size: 12px; cursor: pointer; }
  .attachments small { margin-left: 4px; color: var(--muted); }
  .preview-only { margin: 0 20px 12px; padding: 8px 12px; border-radius: 8px; background: color-mix(in oklab, var(--accent) 10%, transparent); color: var(--muted); font-size: 12.5px; }
  .preview-only kbd { padding: 1px 5px; border: 1px solid var(--border); border-radius: 4px; background: var(--surface-2); font-family: var(--font-mono); font-size: 11px; }
  .remote-images-bar { display: flex; gap: 6px; align-items: center; padding: 7px 20px; color: var(--muted); font-size: 11.5px; }
  .remote-images-bar button { padding: 0; border: 0; background: none; color: var(--accent); font: inherit; cursor: pointer; }
  .plain { white-space: pre-wrap; }
  :global(.mach-html img) { display: block; max-width: 100%; height: auto; border-radius: 6px; }
  :global(.mach-html a) { color: var(--accent); text-decoration: underline; text-underline-offset: 2px; overflow-wrap: anywhere; }
  :global(.mach-html table) { max-width: 100%; border-collapse: collapse; }
  :global(.mach-html blockquote) { margin: 8px 0; padding-left: 12px; border-left: 3px solid var(--border); color: var(--muted); }
  :global(.mach-html pre) { overflow-x: auto; padding: 10px 14px; border-radius: 8px; background: var(--surface-2); font-family: var(--font-mono); font-size: 12.5px; line-height: 1.55; }
  :global(.mach-html code) { padding: 2px 5px; border-radius: 4px; background: var(--surface-2); font-family: var(--font-mono); font-size: .88em; }
  :global(.mach-html h1), :global(.mach-html h2), :global(.mach-html h3), :global(.mach-html h4) { margin: 16px 0 8px; color: var(--text); font-weight: 600; letter-spacing: -.01em; }
  :global(.mach-html p) { margin: 8px 0; line-height: 1.6; }
  :global(.mach-html ul), :global(.mach-html ol) { margin: 8px 0; padding-left: 22px; }
  :global(.mach-html hr) { margin: 16px 0; border: 0; border-top: 1px solid var(--border); }
</style>

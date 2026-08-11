<script lang="ts">
  import type { Message, ThreadSummary } from "../lib/ipc";
  import { linkify } from "../lib/text";
  import { renderEmailHtml } from "../lib/html";
  import { avatarColor, initialsFor } from "../lib/avatar";

  let { v }: { v: { kind: "thread"; thread: ThreadSummary; messages: Message[]; selectedMsg: number } } = $props();
  let overrides = $state<Record<string, boolean>>({});
  let previousSelected = $state<number | null>(null);

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
    return from.match(/<([^>]+)>/)?.[1] ?? "";
  }
</script>

<div class="reader">
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
              <div class="sender"><span>{senderName(message.from)}</span>{#if senderEmail(message.from)}<small>{senderEmail(message.from)}</small>{/if}</div>
              <p>to {message.to.join(", ") || "—"}</p>
              {#if !expanded}<div class="preview">{(message.body_plain ?? message.snippet ?? "").slice(0, 200)}</div>{/if}
            </div>
            <time>{prettyFullDate(message.internal_date)}</time>
          </header>
          {#if expanded}
            {#if message.body_html && message.body_html.length > 0}
              <div class="message-body mach-html">{@html renderEmailHtml(message).html}</div>
            {:else}
              <div class="message-body plain">{@html linkify(message.body_plain ?? message.snippet ?? "(no body)")}</div>
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
  .message-meta p { margin: 2px 0 0; color: var(--muted); font-size: 11.5px; }
  time { flex-shrink: 0; color: var(--muted); font-size: 11px; font-variant-numeric: tabular-nums; }
  .preview { display: -webkit-box; overflow: hidden; margin-top: 8px; color: var(--muted); font-size: 13px; -webkit-box-orient: vertical; -webkit-line-clamp: 2; line-clamp: 2; }
  .message-body { padding: 0 20px 20px; overflow-wrap: anywhere; color: var(--muted); font-size: 14px; line-height: 1.6; }
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

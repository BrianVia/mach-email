// Sanitize HTML email bodies for safe rendering, block remote images by
// default, and rewrite `cid:` references for the WebView's Rust handler.

import DOMPurify from "dompurify";
import type { Message } from "./ipc";

DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  if (node.tagName === "A") {
    node.setAttribute("target", "_blank");
    node.setAttribute("rel", "noopener noreferrer");
  }
});

export type RenderableHtml = {
  html: string;
  blockedRemoteCount: number;
};

const TRANSPARENT_GIF =
  "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

/**
 * Sanitize a message's HTML body and resolve `cid:` references against
 * the message's `inline_images` map. Returns an HTML string safe to drop
 * into a `<div innerHTML={...}>` block.
 *
 * - `<script>`, `<iframe>`, `<object>`, `<embed>`, `<form>`, and on*
 *   event attributes are stripped by DOMPurify.
 * - `<a>` tags get `target="_blank"` + `rel="noopener noreferrer"`.
 * - `<img src="cid:foo">` is rewritten to
 *   `mach://attachment/<account>/<msg_id>/<att_id>` so the Tauri protocol handler
 *   fetches the bytes and serves them with the right Content-Type.
 * - Remote images and remote CSS `url()` references are blocked unless
 *   `showRemote` is true.
 */
export function renderEmailHtml(
  m: Message,
  { showRemote }: { showRemote: boolean } = { showRemote: false },
): RenderableHtml {
  const raw = m.body_html ?? "";
  if (!raw) return { html: "", blockedRemoteCount: 0 };

  const sanitized = DOMPurify.sanitize(raw, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: ["script", "iframe", "object", "embed", "form", "style"],
    FORBID_ATTR: ["onerror", "onload", "onclick", "onmouseover", "onfocus"],
    ALLOW_DATA_ATTR: false,
  });

  // Parse + rewrite image sources.
  const doc = new DOMParser().parseFromString(sanitized, "text/html");
  const blockedRemoteCount = rewriteEmailDocument(doc, m, showRemote);
  return { html: doc.body.innerHTML, blockedRemoteCount };
}

/** @internal Exported so the browser-independent Bun tests can exercise it. */
export function rewriteEmailDocument(
  doc: Document,
  m: Message,
  showRemote = false,
): number {
  const cidMap = new Map<string, string>();
  for (const img of m.inline_images ?? []) {
    cidMap.set(img.content_id.toLowerCase(), img.attachment_id);
  }

  let blockedRemoteCount = 0;
  for (const img of Array.from(doc.querySelectorAll("img[src]"))) {
    const src = img.getAttribute("src") ?? "";
    const normalizedSrc = src.trim();
    if (/^(https?:)?\/\//i.test(normalizedSrc) && !showRemote) {
      img.removeAttribute("srcset");
      img.setAttribute("data-mach-remote-src", src);
      img.setAttribute("src", TRANSPARENT_GIF);
      blockedRemoteCount += 1;
      continue;
    }
    if (!/^cid:/i.test(normalizedSrc)) continue;

    const cid = normalizedSrc.slice(4).toLowerCase().trim().replace(/^<|>$/g, "");
    const attId = cidMap.get(cid);
    if (attId) {
      img.setAttribute(
        "src",
        `mach://attachment/${m.account_id}/${m.id}/${attId}`,
      );
      img.setAttribute("loading", "lazy");
    } else {
      // Couldn't resolve — leave a visible marker rather than a broken icon.
      img.setAttribute("alt", img.getAttribute("alt") ?? "image");
      img.setAttribute("title", `missing cid: ${cid}`);
    }
  }

  if (!showRemote) {
    for (const node of Array.from(doc.querySelectorAll("[style]"))) {
      if (/url\s*\([^)]*https?:/i.test(node.getAttribute("style") ?? "")) {
        node.removeAttribute("style");
      }
    }
  }

  return blockedRemoteCount;
}

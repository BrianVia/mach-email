// Sanitize HTML email bodies for safe rendering. Plus rewrite `cid:`
// references on `<img>` tags to point at our `mach://attachment/...`
// scheme so the WebView can fetch them through our Rust handler.

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
};

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
 * - Remote `<img src="https://...">` is left alone (auto-load, per
 *   project policy — privacy tradeoff the user accepted).
 */
export function renderEmailHtml(m: Message): RenderableHtml {
  const raw = m.body_html ?? "";
  if (!raw) return { html: "" };

  // Build a cid → attachment_id lookup.
  const cidMap = new Map<string, string>();
  for (const img of m.inline_images ?? []) {
    cidMap.set(img.content_id.toLowerCase(), img.attachment_id);
  }

  const sanitized = DOMPurify.sanitize(raw, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: ["script", "iframe", "object", "embed", "form", "style"],
    FORBID_ATTR: ["onerror", "onload", "onclick", "onmouseover", "onfocus"],
    ALLOW_DATA_ATTR: false,
  });

  if (cidMap.size === 0) return { html: sanitized };

  // Parse + rewrite cid: img sources.
  const doc = new DOMParser().parseFromString(sanitized, "text/html");
  for (const img of Array.from(doc.querySelectorAll("img[src^='cid:']"))) {
    const src = img.getAttribute("src") ?? "";
    const cid = src.slice(4).toLowerCase().trim().replace(/^<|>$/g, "");
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
  return { html: doc.body.innerHTML };
}

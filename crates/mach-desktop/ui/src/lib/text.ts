// Render plain-text message bodies safely:
//   1. Escape HTML metachars in the raw text.
//   2. Wrap URLs in real <a> elements so the browser keeps them as one
//      element across visual line wraps (no more "disconnected" links).
//
// Safe because we only inject HTML we constructed ourselves — escaped text
// + anchor tags whose `href` is the matched URL (same string, escaped).

const URL_RE = /(https?:\/\/[^\s<>'"`)\]\}]+)/g;

export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/**
 * Returns an HTML string. Plain text → escaped + URL-linkified.
 * Caller can drop into a `<div innerHTML={...}>` (safe, see above).
 */
export function linkify(text: string): string {
  const out: string[] = [];
  let last = 0;
  let m: RegExpExecArray | null;
  URL_RE.lastIndex = 0;
  while ((m = URL_RE.exec(text)) !== null) {
    out.push(escapeHtml(text.slice(last, m.index)));
    const url = m[0];
    const safeHref = escapeHtml(url);
    out.push(
      `<a href="${safeHref}" target="_blank" rel="noopener noreferrer" class="text-accent underline hover:text-accent-bright">${escapeHtml(url)}</a>`,
    );
    last = m.index + url.length;
  }
  out.push(escapeHtml(text.slice(last)));
  return out.join("");
}

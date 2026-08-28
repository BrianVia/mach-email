import type { Message, ThreadSummary } from "./ipc";

function senderName(from: string): string {
  return from.match(/^\s*"?([^"<]+?)"?\s*<.+>$/)?.[1].trim() ?? from;
}

function localDateTime(value: string): string {
  const date = new Date(value);
  const part = (number: number) => String(number).padStart(2, "0");
  return `${date.getFullYear()}-${part(date.getMonth() + 1)}-${part(date.getDate())} ${part(date.getHours())}:${part(date.getMinutes())}`;
}

function htmlToMarkdown(html: string): string {
  const document = new DOMParser().parseFromString(html, "text/html");

  function walk(node: Node): string {
    if (node.nodeType === Node.TEXT_NODE) return node.textContent?.replace(/\s+/g, " ") ?? "";
    if (!(node instanceof HTMLElement)) return "";
    const tag = node.tagName.toLowerCase();
    if (["style", "script", "head"].includes(tag)) return "";
    const text = Array.from(node.childNodes, walk).join("");

    if (tag === "a") {
      const href = node.getAttribute("href");
      const label = text.trim();
      return href ? (label === href ? href : `[${label}](${href})`) : text;
    }
    if (tag === "strong" || tag === "b") return `**${text}**`;
    if (tag === "em" || tag === "i") return `*${text}*`;
    if (/^h[1-4]$/.test(tag)) return `${"#".repeat(Number(tag[1]))} ${text.trim()}\n\n`;
    if (tag === "li") {
      const siblings = Array.from(node.parentElement?.children ?? []).filter((child) => child.tagName === "LI");
      const marker = node.parentElement?.tagName === "OL" ? `${siblings.indexOf(node) + 1}.` : "-";
      return `${marker} ${text.trim()}\n`;
    }
    if (tag === "blockquote") {
      return `${text.trim().split("\n").map((line) => `> ${line}`).join("\n")}\n\n`;
    }
    if (tag === "br" || tag === "tr") return `${text}\n`;
    if (tag === "p" || tag === "div") return `${text}\n\n`;
    return text;
  }

  return walk(document.body).replace(/\n{3,}/g, "\n\n").trim();
}

export function threadToMarkdown(thread: ThreadSummary, messages: Message[]): string {
  const heading = thread.subject || "(no subject)";
  const intro = `> mach thread id: \`${thread.id}\` (account ${thread.account_id}) — an agent can open it with the mach MCP \`open_thread\` action.`;
  const sections = messages.map((message) => {
    const body = message.body_plain ?? (message.body_html !== null ? htmlToMarkdown(message.body_html) : message.snippet ?? "");
    return `## ${senderName(message.from)} — ${localDateTime(message.internal_date)}\nFrom: ${message.from} | To: ${message.to.join(", ")}\n\n${body}`;
  });
  return `# ${heading}\n\n${intro}\n\n${sections.join("\n\n---\n\n")}`;
}

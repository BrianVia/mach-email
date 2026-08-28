import { expect, test } from "bun:test";
import type { Message, ThreadSummary } from "./ipc";
import { threadToMarkdown } from "./markdown";

test("formats a thread with its verbatim plain-text body", () => {
  const thread = { id: "thread-1", account_id: "me@example.com", subject: "Status" } as ThreadSummary;
  const message = {
    from: "Ada Lovelace <ada@example.com>",
    to: ["me@example.com", "team@example.com"],
    internal_date: "2026-08-28T14:05:00",
    body_plain: "First line\n\nSecond line",
  } as Message;

  expect(threadToMarkdown(thread, [message])).toBe(`# Status

> mach thread id: \`thread-1\` (account me@example.com) — an agent can open it with the mach MCP \`open_thread\` action.

## Ada Lovelace — 2026-08-28 14:05
From: Ada Lovelace <ada@example.com> | To: me@example.com, team@example.com

First line

Second line`);
});

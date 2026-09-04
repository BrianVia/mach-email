import { expect, test } from "bun:test";
import type { ThreadSummary } from "./ipc";
import { batchNotifications } from "./notify";

const thread = (id: string, subject = `Subject ${id}`) => ({
  id,
  subject,
  snippet: `Snippet ${id}`,
  participants: [`Sender ${id} <${id}@example.com>`],
}) as ThreadSummary;

test("batches three thread notifications and collapses the rest", () => {
  const notifications = batchNotifications([
    thread("1"),
    thread("2", ""),
    thread("3"),
    thread("4"),
    thread("5"),
  ]);

  expect(notifications).toEqual([
    { title: "Sender 1", body: "Subject 1", threadId: "1" },
    { title: "Sender 2", body: "Snippet 2", threadId: "2" },
    { title: "Sender 3", body: "Subject 3", threadId: "3" },
    { title: "2 more new messages", threadId: "1" },
  ]);
});

import type { ThreadSummary } from "./ipc";

export type NotificationBatch = {
  title: string;
  body?: string;
  threadId: string;
};

function senderName(from: string): string {
  return from.match(/^\s*"?([^"<]+?)"?\s*<.+>$/)?.[1].trim() ?? from;
}

export function batchNotifications(threads: ThreadSummary[]): NotificationBatch[] {
  const notifications: NotificationBatch[] = threads.slice(0, 3).map((thread) => ({
    title: senderName(thread.participants[0] ?? "New message"),
    body: thread.subject || thread.snippet,
    threadId: thread.id,
  }));
  if (threads.length > 3) {
    notifications.push({
      title: `${threads.length - 3} more new messages`,
      threadId: threads[0].id,
    });
  }
  return notifications;
}

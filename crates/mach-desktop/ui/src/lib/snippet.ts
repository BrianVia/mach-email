export type SnippetToken = { start: number; query: string };

export function detectSnippetToken(text: string, cursor: number): SnippetToken | null {
  const before = text.slice(0, cursor);
  const start = before.lastIndexOf(";");
  if (start < 0 || (start > 0 && !/\s/.test(text[start - 1])) || /\s/.test(before.slice(start + 1))) {
    return null;
  }
  return { start, query: before.slice(start + 1) };
}

export type Split = "important" | "other" | "newsletters";

export function splitOf(labelIds: string[]): Split {
  const has = (label: string) => labelIds.includes(label);
  if (["CATEGORY_PROMOTIONS", "CATEGORY_UPDATES", "CATEGORY_FORUMS"].some(has)) return "newsletters";
  if (has("IMPORTANT") || has("CATEGORY_PERSONAL") || !labelIds.some((label) => label.startsWith("CATEGORY_"))) return "important";
  return "other";
}

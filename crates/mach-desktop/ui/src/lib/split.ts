export type Split = "important" | "other" | "updates" | "newsletters";

export function splitOf(labelIds: string[]): Split {
  const has = (label: string) => labelIds.includes(label);
  if (has("CATEGORY_UPDATES")) return "updates";
  if (["CATEGORY_PROMOTIONS", "CATEGORY_FORUMS"].some(has)) return "newsletters";
  if (has("IMPORTANT") || has("CATEGORY_PERSONAL") || !labelIds.some((label) => label.startsWith("CATEGORY_"))) return "important";
  return "other";
}

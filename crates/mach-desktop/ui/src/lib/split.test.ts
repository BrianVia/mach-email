import { describe, expect, test } from "bun:test";
import { splitOf } from "./split";

describe("splitOf", () => {
  test.each([
    [["INBOX", "IMPORTANT"], "important"],
    [["IMPORTANT", "CATEGORY_PROMOTIONS"], "newsletters"],
    [["CATEGORY_SOCIAL"], "other"],
  ] as const)("classifies %j as %s", (labels, split) => {
    expect(splitOf([...labels])).toBe(split);
  });
});

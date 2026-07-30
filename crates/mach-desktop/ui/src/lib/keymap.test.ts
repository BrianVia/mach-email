import { describe, expect, test } from "bun:test";
import { Keymap } from "./keymap";

describe("Keymap", () => {
  test("merges a partial user keymap over defaults", () => {
    const defaults = Keymap.fromToml(`
      [normal]
      j = "select_next"
      k = "select_prev"
    `);
    const user = Keymap.fromToml(`
      [normal]
      j = "select_prev"
    `);

    defaults.merge(user);

    expect(defaults.resolve("normal", "j", { selection: [] })).toEqual({
      kind: "action",
      action: { kind: "select_prev" },
    });
    expect(defaults.resolve("normal", "k", { selection: [] })).toEqual({
      kind: "action",
      action: { kind: "select_prev" },
    });
  });
});

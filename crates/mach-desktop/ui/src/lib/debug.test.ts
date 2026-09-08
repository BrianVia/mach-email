import { expect, test } from "bun:test";
import { stringifyArgs } from "./debug";

test("stringifyArgs handles circular values and caps output", () => {
  const circular: { self?: unknown } = {};
  circular.self = circular;
  expect(stringifyArgs([circular, "abcdef"], 10)).toBe("[object Ob");
});

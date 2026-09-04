import { describe, expect, test } from "bun:test";
import { detectSnippetToken } from "./snippet";

describe("detectSnippetToken", () => {
  test("detects a token at a line start or after whitespace", () => {
    expect(detectSnippetToken(";tha", 4)).toEqual({ start: 0, query: "tha" });
    expect(detectSnippetToken("Hello\n;eta", 10)).toEqual({ start: 6, query: "eta" });
    expect(detectSnippetToken("Hello ;thanks", 13)).toEqual({ start: 6, query: "thanks" });
  });

  test("rejects tokens after text and tokens containing whitespace", () => {
    expect(detectSnippetToken("word;tha", 8)).toBeNull();
    expect(detectSnippetToken(";two words", 10)).toBeNull();
  });
});

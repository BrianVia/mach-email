import { expect, mock, test } from "bun:test";
import type { Message } from "./ipc";

mock.module("dompurify", () => ({ default: { addHook() {} } }));

class TestElement {
  constructor(private attrs: Record<string, string>) {}
  getAttribute(name: string) { return this.attrs[name] ?? null; }
  setAttribute(name: string, value: string) { this.attrs[name] = value; }
  removeAttribute(name: string) { delete this.attrs[name]; }
}

function message(inline_images: Message["inline_images"] = []): Message {
  return { account_id: "me@example.com", id: "message-1", inline_images } as Message;
}

function documentWith(images: TestElement[] = [], styled: TestElement[] = []): Document {
  return {
    querySelectorAll: (selector: string) => selector === "img[src]" ? images : styled,
  } as unknown as Document;
}

test("neutralizes and counts remote images", async () => {
  const { rewriteEmailDocument } = await import("./html");
  const image = new TestElement({ src: "https://tracker.example/pixel.gif" });

  expect(rewriteEmailDocument(documentWith([image]), message())).toBe(1);
  expect(image.getAttribute("data-mach-remote-src")).toBe("https://tracker.example/pixel.gif");
  expect(image.getAttribute("src")).toStartWith("data:image/gif;base64,");
});

test("still rewrites cid images", async () => {
  const { rewriteEmailDocument } = await import("./html");
  const image = new TestElement({ src: "cid:Logo" });
  const inlineImages = [{ content_id: "logo", attachment_id: "attachment-1" }] as Message["inline_images"];

  rewriteEmailDocument(documentWith([image]), message(inlineImages));

  expect(image.getAttribute("src")).toBe("mach://attachment/me@example.com/message-1/attachment-1");
});

test("leaves remote images visible when requested", async () => {
  const { rewriteEmailDocument } = await import("./html");
  const image = new TestElement({ src: "https://example.com/photo.jpg" });

  expect(rewriteEmailDocument(documentWith([image]), message(), true)).toBe(0);
  expect(image.getAttribute("src")).toBe("https://example.com/photo.jpg");
});

test("strips styles containing remote url references", async () => {
  const { rewriteEmailDocument } = await import("./html");
  const styled = new TestElement({ style: "color: red; background: url(https://tracker.example/pixel.gif)" });

  rewriteEmailDocument(documentWith([], [styled]), message());

  expect(styled.getAttribute("style")).toBeNull();
});

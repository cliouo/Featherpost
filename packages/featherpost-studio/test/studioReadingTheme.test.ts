import { expect, it } from "vitest";
import { readingDocumentUrl } from "../src/readingTheme";

it("changes only the isolated document's display query, not its capability or privacy", () => {
  const original =
    "featherpost://localhost/doc/0123456789abcdef?zoom=1#section";
  const themed = new URL(readingDocumentUrl(original, true));
  expect(themed.protocol).toBe("featherpost:");
  expect(themed.pathname).toBe("/doc/0123456789abcdef");
  expect(themed.searchParams.get("appearance")).toBe("dark");
  expect(themed.searchParams.get("zoom")).toBe("1");
  expect(themed.searchParams.has("allow_remote")).toBe(false);
  expect(themed.hash).toBe("#section");
  expect(readingDocumentUrl(original, false)).toBe(original);
});

it("supports the HTTP custom-protocol shape without duplicating the appearance query", () => {
  const original = "http://featherpost.localhost/doc/token?appearance=dark";
  expect(readingDocumentUrl(original, true)).toBe(original);
});

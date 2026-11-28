import { expect, it } from "vitest";
import {
  readingDocumentUrl,
  translatedReadingDocumentUrl,
} from "../src/readingTheme";

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

it("builds translated document URLs cleanly for both Unix scheme and Windows HTTP forms without duplicate query keys", () => {
  const unixBase = "featherpost-mail://localhost/doc/tok1";
  const winBase = "http://featherpost-mail.localhost/doc/tok1";

  expect(
    readingDocumentUrl(unixBase, false, { view: "bilingual", rev: 2 }),
  ).toBe("featherpost-mail://localhost/doc/tok1?translate=bilingual&rev=2");
  expect(
    readingDocumentUrl(winBase, true, { view: "translated", rev: 3 }),
  ).toBe(
    "http://featherpost-mail.localhost/doc/tok1?translate=translated&rev=3&appearance=dark",
  );

  const result = {
    rev: 1,
    bilingual_url: "featherpost-mail://localhost/doc/tok1?translate=bilingual&rev=1",
    translated_url: "featherpost-mail://localhost/doc/tok1?translate=translated&rev=1",
    bilingual_dark_url:
      "featherpost-mail://localhost/doc/tok1?translate=bilingual&rev=1&appearance=dark",
    translated_dark_url:
      "featherpost-mail://localhost/doc/tok1?translate=translated&rev=1&appearance=dark",
  };
  expect(translatedReadingDocumentUrl(unixBase, result, "bilingual", false)).toBe(
    result.bilingual_url,
  );
  expect(translatedReadingDocumentUrl(unixBase, result, "translated_only", true)).toBe(
    result.translated_dark_url,
  );
});

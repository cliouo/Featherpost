import type {
  TranslationDisplayMode,
  UiHtmlTranslationResult,
} from "./bridge/translation";

export interface ReadingDocumentTranslationOptions {
  view: "bilingual" | "translated";
  rev: number;
}

/** Keep the same capability and privacy decision; only change its display palette and/or translation view. */
export function readingDocumentUrl(
  original: string,
  dark: boolean,
  translation?: ReadingDocumentTranslationOptions | null,
) {
  if (!dark && !translation) return original;
  const url = new URL(original);
  if (translation) {
    url.searchParams.set("translate", translation.view);
    url.searchParams.set("rev", String(translation.rev));
  }
  if (dark) {
    url.searchParams.set("appearance", "dark");
  } else if (translation) {
    url.searchParams.delete("appearance");
  }
  return url.href;
}

/**
 * Resolve the isolated `featherpost-mail://` (or Windows `http://...localhost`) URL for a
 * translated HTML document without duplicating query keys or issuing any outbound request.
 */
export function translatedReadingDocumentUrl(
  originalUrl: string,
  result: Pick<
    UiHtmlTranslationResult,
    "rev" | "bilingual_url" | "translated_url" | "bilingual_dark_url" | "translated_dark_url"
  >,
  displayMode: TranslationDisplayMode,
  dark: boolean,
): string {
  const prebuilt =
    displayMode === "translated_only"
      ? dark
        ? result.translated_dark_url
        : result.translated_url
      : dark
        ? result.bilingual_dark_url
        : result.bilingual_url;
  if (prebuilt) return prebuilt;
  return readingDocumentUrl(originalUrl, dark, {
    view: displayMode === "translated_only" ? "translated" : "bilingual",
    rev: result.rev,
  });
}

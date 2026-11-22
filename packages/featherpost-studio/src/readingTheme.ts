/** Keep the same capability and privacy decision; only change its display palette. */
export function readingDocumentUrl(original: string, dark: boolean) {
  if (!dark) return original;
  const url = new URL(original);
  url.searchParams.set("appearance", "dark");
  return url.href;
}

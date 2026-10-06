/** Accessibility labels compared case- and mnemonic-insensitively. */
export function normalizeLabel(text: string): string {
  return text
    .normalize("NFKC")
    .toLowerCase()
    .replace(/&&/gu, "")
    .replace(/&/gu, "")
    .replace(//gu, "&")
    .replace(/\s+/gu, " ")
    .trim()
    .replace(/(?:\.\.\.|…)+$/u, "")
    .trim();
}

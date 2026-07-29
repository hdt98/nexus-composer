/**
 * Normalizes common CJK, full-width, and curly quotation marks to ASCII to
 * prevent TOML parse failures.
 * - Double quotes: ” “ „ ‟ ＂ → "
 * - Single quotes: ’ ‘ ＇ → '
 * Book-title and corner brackets are intentionally preserved to avoid changing
 * content semantics.
 */
export const normalizeQuotes = (text: string): string => {
  if (!text) return text;
  return (
    text
      // Double-quote variants → "
      .replace(/[“”„‟＂]/g, '"')
      // Single-quote variants → '
      .replace(/[‘’＇]/g, "'")
  );
};

/**
 * Normalizes TOML text. Currently equivalent to normalizeQuotes and may later
 * expand to whitespace or line-ending normalization.
 */
export const normalizeTomlText = (text: string): string =>
  normalizeQuotes(text);

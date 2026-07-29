/**
 * Validates a custom User-Agent.
 *
 * Matches backend `parse_custom_user_agent`, based on
 * `http::HeaderValue::from_str`, exactly. HeaderValue validates bytes using
 * `b >= 32 && b != 127 || b == '\t'`, so tabs, visible ASCII (0x20–0x7E), and
 * all non-ASCII characters whose UTF-8 bytes are at least 0x80 are valid. Only
 * control characters are invalid: 0x00–0x1F except tab, including newlines,
 * and 0x7F (DEL).
 *
 * A string empty after trimming is treated as unset and is valid.
 */
export function isValidUserAgentHeader(value: string): boolean {
  const trimmed = value.trim();
  if (trimmed === "") return true;
  // eslint-disable-next-line no-control-regex
  return !/[\x00-\x08\x0a-\x1f\x7f]/.test(trimmed);
}

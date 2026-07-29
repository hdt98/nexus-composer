import { describe, it, expect } from "vitest";
import { isValidUserAgentHeader } from "@/lib/userAgent";

// Matches the byte-level rules used by backend parse_custom_user_agent and
// http::HeaderValue::from_str. Valid bytes satisfy b>=32 && b!=127 || b=='\t',
// so only control characters other than tab are invalid. Visible ASCII and
// non-ASCII are valid. Construct control characters with String.fromCharCode
// to avoid embedding raw bytes in source.
const NUL = String.fromCharCode(0);
const DEL = String.fromCharCode(0x7f);

describe("isValidUserAgentHeader", () => {
  it("treats empty / whitespace-only as valid (unset)", () => {
    expect(isValidUserAgentHeader("")).toBe(true);
    expect(isValidUserAgentHeader("   ")).toBe(true);
  });

  it("accepts visible ASCII (trimmed)", () => {
    expect(isValidUserAgentHeader("claude-cli/2.1.161")).toBe(true);
    expect(isValidUserAgentHeader("  claude-cli/2.1.161  ")).toBe(true);
  });

  it("accepts non-ASCII — matches backend HeaderValue byte rule", () => {
    expect(isValidUserAgentHeader("claude-cli/1.0 Tiếng Việt")).toBe(true);
  });

  it("accepts internal tab", () => {
    expect(isValidUserAgentHeader("claude\tcli")).toBe(true);
  });

  it("rejects control characters (newline / null / DEL)", () => {
    expect(isValidUserAgentHeader("claude\ncli")).toBe(false);
    expect(isValidUserAgentHeader(`claude${NUL}cli`)).toBe(false);
    expect(isValidUserAgentHeader(`claude${DEL}cli`)).toBe(false);
  });
});

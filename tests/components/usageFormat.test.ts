import { describe, expect, it } from "vitest";
import {
  formatTokensShort,
  getLocaleFromLanguage,
} from "@/components/usage/format";

describe("usage format helpers", () => {
  it("formats English token units with K/M/B suffixes", () => {
    expect(formatTokensShort(12_345, "en")).toBe("12.3K");
    expect(formatTokensShort(123_456_789, "en", 2)).toBe("123.46M");
  });

  it("formats Vietnamese token units with nghin/trieu/ty suffixes", () => {
    expect(formatTokensShort(12_345, "vi")).toBe("12.3 nghin");
    expect(formatTokensShort(123_456_789, "vi", 2)).toBe("123.46 trieu");
  });

  it("resolves English and Vietnamese locale aliases", () => {
    expect(getLocaleFromLanguage("en")).toBe("en-US");
    expect(getLocaleFromLanguage("vi")).toBe("vi-VN");
    expect(getLocaleFromLanguage("vi_VN")).toBe("vi-VN");
    expect(getLocaleFromLanguage("en-US")).toBe("en-US");
  });
});

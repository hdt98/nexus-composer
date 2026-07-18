import { describe, expect, it } from "vitest";
import {
  buildLocalProxyRequestOverrides,
  hasInvalidMaxOutputTokens,
  isProtectedLocalProxyHeaderName,
  isValidHttpHeaderName,
  isValidHttpHeaderValue,
  parseBodyOverrideJson,
  parseHeaderOverrideJson,
  parseRequestOverrideJson,
} from "@/lib/requestOverrides";

describe("requestOverrides", () => {
  it("treats empty JSON fields as unset", () => {
    expect(buildLocalProxyRequestOverrides("", "   ")).toEqual({});
  });

  it("parses header and body override objects", () => {
    expect(
      buildLocalProxyRequestOverrides(
        '{ "X-Test": "ok" }',
        '{ "temperature": 0.2 }',
      ),
    ).toEqual({
      overrides: {
        headers: { "x-test": "ok" },
        body: { temperature: 0.2 },
      },
    });
  });

  it("rejects non-object body overrides", () => {
    expect(parseRequestOverrideJson("[]").error).toBeTruthy();
  });

  it("rejects non-string header values", () => {
    expect(parseHeaderOverrideJson('{ "X-Test": 1 }').error).toBeTruthy();
  });

  it("rejects invalid header names", () => {
    expect(isValidHttpHeaderName("X-Test")).toBe(true);
    expect(isValidHttpHeaderName("X Foo")).toBe(false);
    expect(isValidHttpHeaderName("Authorization:")).toBe(false);
    expect(parseHeaderOverrideJson('{ "X Foo": "bar" }').error).toBeTruthy();
  });

  it("rejects duplicate header names after case normalization", () => {
    expect(
      parseHeaderOverrideJson('{ "X-Foo": "a", "x-foo": "b" }').error,
    ).toBeTruthy();
  });

  it("rejects protected proxy-managed header names", () => {
    expect(isProtectedLocalProxyHeaderName("Content-Type")).toBe(true);
    expect(isProtectedLocalProxyHeaderName("authorization")).toBe(true);
    expect(isProtectedLocalProxyHeaderName("X-Test")).toBe(false);
    expect(
      parseHeaderOverrideJson('{ "content-type": "text/plain" }').error,
    ).toBeTruthy();
    expect(
      parseHeaderOverrideJson('{ "Authorization": "Bearer x" }').error,
    ).toBeTruthy();
  });

  it("matches backend header value control-character rule", () => {
    expect(isValidHttpHeaderValue("hello\tworld")).toBe(true);
    expect(isValidHttpHeaderValue("hello\nworld")).toBe(false);
  });

  it("rejects stream in body overrides", () => {
    expect(parseBodyOverrideJson('{ "stream": true }').error).toBeTruthy();
    expect(
      buildLocalProxyRequestOverrides("", '{ "stream": false }').error,
    ).toBeTruthy();
  });

  it.each([
    [32768, false],
    [0, true],
    [1.5, true],
    ["4096", true],
    [Number.MAX_SAFE_INTEGER + 1, true],
  ])(
    "classifies max_tokens %j as invalid=%s without changing generic parsing",
    (maxTokens, expectedInvalid) => {
      const parsed = parseBodyOverrideJson(
        JSON.stringify({ max_tokens: maxTokens }),
      );
      expect(parsed.error).toBeUndefined();
      expect(hasInvalidMaxOutputTokens(parsed.value)).toBe(expectedInvalid);
    },
  );
});

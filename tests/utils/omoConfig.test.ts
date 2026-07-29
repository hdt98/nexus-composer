import { describe, expect, it } from "vitest";
import {
  buildOmoProfilePreview,
  buildOmoSlimProfilePreview,
  OMO_SLIM_BUILTIN_AGENTS,
  OMO_SLIM_DISABLEABLE_AGENTS,
  parseOmoOtherFieldsObject,
} from "@/types/omo";

describe("parseOmoOtherFieldsObject", () => {
  it("parses a JSON object", () => {
    expect(parseOmoOtherFieldsObject('{ "foo": 1 }')).toEqual({ foo: 1 });
  });

  it("returns undefined for arrays and strings", () => {
    expect(parseOmoOtherFieldsObject('["a"]')).toBeUndefined();
    expect(parseOmoOtherFieldsObject('"hello"')).toBeUndefined();
  });

  it("throws for invalid JSON", () => {
    expect(() => parseOmoOtherFieldsObject("{")).toThrow();
  });
});

describe("buildOmoProfilePreview", () => {
  it("merges object-valued otherFields and ignores arrays", () => {
    const fromArray = buildOmoProfilePreview({}, {}, '["a", "b"]');
    expect(fromArray).toEqual({});

    const fromObject = buildOmoProfilePreview({}, {}, '{ "foo": "bar" }');
    expect(fromObject).toEqual({ foo: "bar" });
  });
});

describe("buildOmoSlimProfilePreview", () => {
  it("preserves top-level council config and writes the council agent model", () => {
    const preview = buildOmoSlimProfilePreview(
      {
        council: { model: "openai/gpt-5.4-mini" },
      },
      '{ "council": { "default_preset": "default" }, "fallback": { "enabled": true } }',
    );

    expect(preview).toEqual({
      council: { default_preset: "default" },
      fallback: { enabled: true },
      agents: {
        council: { model: "openai/gpt-5.4-mini" },
      },
    });
  });
});

describe("OMO Slim metadata", () => {
  it("treats council as a built-in, disableable agent", () => {
    expect(OMO_SLIM_BUILTIN_AGENTS).toContainEqual(
      expect.objectContaining({
        key: "council",
        display: "Council",
        group: "sub",
      }),
    );
    expect(OMO_SLIM_DISABLEABLE_AGENTS).toContainEqual({
      value: "council",
      label: "Council",
    });
  });
});

import { renderHook } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import { useCodexConfigState } from "@/components/providers/forms/hooks/useCodexConfigState";

// Regression: editing an existing native Responses provider must retain hidden
// modelCatalog fields (supportsParallelToolCalls, inputModalities, and
// baseInstructions). Otherwise saving strips official base_instructions,
// parallel-tool support, and image modalities from the generated Codex catalog.
//
// initialData must have stable identity because the hook's initialization
// effect depends on [initialData]. An inline literal creates a new reference on
// every render, repeatedly calls setState, and causes an infinite loop and OOM.
describe("useCodexConfigState catalog load", () => {
  it("preserves native-profile hidden fields (camelCase, DB SSOT)", () => {
    const initialData = {
      settingsConfig: {
        auth: { OPENAI_API_KEY: "sk-x" },
        config: "",
        modelCatalog: {
          models: [
            {
              model: "MiniMax-M3",
              displayName: "MiniMax-M3",
              contextWindow: 1000000,
              supportsParallelToolCalls: true,
              inputModalities: ["text", "image"],
              baseInstructions: "You are Codex, based on MiniMax-M3.",
            },
          ],
        },
      },
    };

    const { result } = renderHook(() => useCodexConfigState({ initialData }));

    expect(result.current.codexCatalogModels).toEqual([
      {
        model: "MiniMax-M3",
        displayName: "MiniMax-M3",
        contextWindow: 1000000,
        supportsParallelToolCalls: true,
        inputModalities: ["text", "image"],
        baseInstructions: "You are Codex, based on MiniMax-M3.",
      },
    ]);
  });

  it("maps snake_case hidden fields (live reverse-parse fallback) to camelCase", () => {
    const initialData = {
      settingsConfig: {
        auth: {},
        config: "",
        modelCatalog: {
          models: [
            {
              model: "mimo-v2.5-pro",
              display_name: "MiMo V2.5 Pro",
              context_window: 262144,
              supports_parallel_tool_calls: false,
              input_modalities: ["text"],
              base_instructions: "You are MiMo, developed by Xiaomi.",
            },
          ],
        },
      },
    };

    const { result } = renderHook(() => useCodexConfigState({ initialData }));

    expect(result.current.codexCatalogModels).toEqual([
      {
        model: "mimo-v2.5-pro",
        displayName: "MiMo V2.5 Pro",
        contextWindow: 262144,
        supportsParallelToolCalls: false,
        inputModalities: ["text"],
        baseInstructions: "You are MiMo, developed by Xiaomi.",
      },
    ]);
  });
});

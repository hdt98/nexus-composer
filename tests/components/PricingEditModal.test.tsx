import { render, screen, fireEvent } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { PricingEditModal } from "@/components/usage/PricingEditModal";
import type { ModelPricing } from "@/types/usage";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, options?: string | { defaultValue?: string }) =>
      typeof options === "string" ? options : (options?.defaultValue ?? key),
  }),
}));

vi.mock("sonner", () => ({
  toast: {
    success: vi.fn(),
    error: vi.fn(),
  },
}));

vi.mock("@/lib/query/usage", () => ({
  useUpdateModelPricing: () => ({
    mutateAsync: vi.fn(),
    isPending: false,
  }),
}));

vi.mock("@/components/common/FullScreenPanel", () => ({
  FullScreenPanel: ({
    children,
    footer,
  }: {
    children: React.ReactNode;
    footer?: React.ReactNode;
  }) => (
    <div>
      {children}
      {footer}
    </div>
  ),
}));

const model: ModelPricing = {
  modelId: "deepseek-v4",
  displayName: "DeepSeek V4",
  inputCostPerMillion: "1",
  outputCostPerMillion: "3",
  cacheReadCostPerMillion: "0.0028",
  cacheCreationCostPerMillion: "0",
};

const PRICE_FIELDS = [
  { id: "inputCost", label: "Input Cost (per million tokens, USD)" },
  { id: "outputCost", label: "Output Cost (per million tokens, USD)" },
  {
    id: "cacheReadCost",
    label: "Cache Read Cost (per million tokens, USD)",
  },
  {
    id: "cacheCreationCost",
    label: "Cache Write Cost (per million tokens, USD)",
  },
] as const;

describe("PricingEditModal", () => {
  it("all price inputs have step=0.0001", () => {
    render(<PricingEditModal open model={model} onClose={() => {}} />);

    for (const { id, label } of PRICE_FIELDS) {
      const input = screen.getByLabelText(label, {
        selector: `#${id}`,
      }) as HTMLInputElement;
      expect(input).toHaveAttribute("step", "0.0001");
    }
  });

  it("accepts precise cache read cost like 0.0028", () => {
    render(<PricingEditModal open model={model} onClose={() => {}} />);

    const cacheReadInput = document.getElementById(
      "cacheReadCost",
    ) as HTMLInputElement;
    expect(cacheReadInput.value).toBe("0.0028");
    expect(cacheReadInput.checkValidity()).toBe(true);
  });

  it("allows user to input sub-cent prices via change event", () => {
    render(<PricingEditModal open model={model} onClose={() => {}} isNew />);

    const cacheReadInput = document.getElementById(
      "cacheReadCost",
    ) as HTMLInputElement;

    fireEvent.change(cacheReadInput, { target: { value: "0.0015" } });
    expect(cacheReadInput.value).toBe("0.0015");
  });
});

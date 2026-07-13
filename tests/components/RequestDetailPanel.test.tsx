import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { RequestDetailPanel } from "@/components/usage/RequestDetailPanel";

const useRequestDetailMock = vi.hoisted(() => vi.fn());

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, fallback?: string | { defaultValue?: string }) =>
      (typeof fallback === "string" ? fallback : fallback?.defaultValue) ?? key,
    i18n: { language: "en" },
  }),
}));

vi.mock("@/lib/query/usage", () => ({
  useRequestDetail: (...args: unknown[]) => useRequestDetailMock(...args),
}));

vi.mock("@/components/ui/dialog", () => ({
  Dialog: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogContent: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogHeader: ({ children }: { children: React.ReactNode }) => (
    <div>{children}</div>
  ),
  DialogTitle: ({ children }: { children: React.ReactNode }) => (
    <h2>{children}</h2>
  ),
}));

describe("RequestDetailPanel", () => {
  it("labels token and cost data unavailable while keeping observed timing", () => {
    useRequestDetailMock.mockReturnValue({
      data: {
        requestId: "unknown-usage",
        providerId: "nexus",
        providerName: "Nexus",
        appType: "codex",
        model: "glm-5.2",
        pricingModel: "glm-5.2",
        tokenUsageKnown: false,
        pricingKnown: false,
        costMultiplier: "1",
        inputTokens: 0,
        outputTokens: 0,
        cacheReadTokens: 0,
        cacheCreationTokens: 0,
        inputCostUsd: "0",
        outputCostUsd: "0",
        cacheReadCostUsd: "0",
        cacheCreationCostUsd: "0",
        totalCostUsd: "0",
        isStreaming: true,
        latencyMs: 2_500,
        firstTokenMs: 300,
        statusCode: 200,
        createdAt: 1_725_555_120,
        dataSource: "proxy",
      },
      isLoading: false,
      isError: false,
      refetch: vi.fn(),
    });

    render(<RequestDetailPanel requestId="unknown-usage" onClose={vi.fn()} />);

    expect(
      screen.getByText(/Token totals and estimated costs exclude this request/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(
        /N\/A because the upstream response did not report token usage/,
      ),
    ).toBeInTheDocument();
    expect(screen.getByText("2500ms")).toBeInTheDocument();
    expect(screen.getByText("300ms")).toBeInTheDocument();
    expect(screen.getByText("200")).toBeInTheDocument();
    expect(screen.queryByText("$0.000000")).not.toBeInTheDocument();
  });

  it("hides placeholder zeros when token usage is known but pricing is not", () => {
    useRequestDetailMock.mockReturnValue({
      data: {
        requestId: "known-unpriced",
        providerId: "nexus",
        providerName: "Nexus",
        appType: "codex",
        model: "custom-model",
        pricingModel: "custom-model",
        tokenUsageKnown: true,
        pricingKnown: false,
        costMultiplier: "1",
        inputTokens: 100,
        outputTokens: 20,
        cacheReadTokens: 0,
        cacheCreationTokens: 0,
        inputCostUsd: "0",
        outputCostUsd: "0",
        cacheReadCostUsd: "0",
        cacheCreationCostUsd: "0",
        totalCostUsd: "0",
        isStreaming: false,
        latencyMs: 500,
        statusCode: 200,
        createdAt: 1_725_555_120,
        dataSource: "proxy",
      },
      isLoading: false,
      isError: false,
      refetch: vi.fn(),
    });

    render(<RequestDetailPanel requestId="known-unpriced" onClose={vi.fn()} />);

    expect(
      screen.getByText(/no configured pricing rule matched/),
    ).toBeVisible();
    expect(screen.getByText("100")).toBeInTheDocument();
    expect(screen.queryByText("$0.000000")).not.toBeInTheDocument();
  });

  it("renders zero costs for an explicitly priced free request", () => {
    useRequestDetailMock.mockReturnValue({
      data: {
        requestId: "known-free",
        providerId: "nexus",
        providerName: "Nexus",
        appType: "codex",
        model: "free-model",
        pricingModel: "free-model",
        tokenUsageKnown: true,
        pricingKnown: true,
        costMultiplier: "1",
        inputTokens: 100,
        outputTokens: 20,
        cacheReadTokens: 0,
        cacheCreationTokens: 0,
        inputCostUsd: "0",
        outputCostUsd: "0",
        cacheReadCostUsd: "0",
        cacheCreationCostUsd: "0",
        totalCostUsd: "0",
        isStreaming: false,
        latencyMs: 500,
        statusCode: 200,
        createdAt: 1_725_555_120,
        dataSource: "proxy",
      },
      isLoading: false,
      isError: false,
      refetch: vi.fn(),
    });

    render(<RequestDetailPanel requestId="known-free" onClose={vi.fn()} />);

    expect(screen.getAllByText("$0.000000").length).toBeGreaterThan(0);
    expect(
      screen.queryByText(/no configured pricing rule matched/),
    ).not.toBeInTheDocument();
  });

  it("labels an indeterminate legacy zero without calling it free or unpriced", () => {
    useRequestDetailMock.mockReturnValue({
      data: {
        requestId: "legacy-unknown",
        providerId: "nexus",
        providerName: "Nexus",
        appType: "codex",
        model: "legacy-model",
        pricingModel: "legacy-model",
        tokenUsageKnown: true,
        pricingKnown: null,
        costMultiplier: "1",
        inputTokens: 100,
        outputTokens: 20,
        cacheReadTokens: 0,
        cacheCreationTokens: 0,
        inputCostUsd: "0",
        outputCostUsd: "0",
        cacheReadCostUsd: "0",
        cacheCreationCostUsd: "0",
        totalCostUsd: "0",
        isStreaming: false,
        latencyMs: 500,
        statusCode: 200,
        createdAt: 1_725_555_120,
        dataSource: "proxy",
      },
      isLoading: false,
      isError: false,
      refetch: vi.fn(),
    });

    render(<RequestDetailPanel requestId="legacy-unknown" onClose={vi.fn()} />);

    expect(
      screen.getByText(/Pricing is unknown for this legacy request/),
    ).toBeVisible();
    expect(screen.queryByText("$0.000000")).not.toBeInTheDocument();
    expect(screen.queryByText(/^Unpriced$/)).not.toBeInTheDocument();
  });

  it("uses the pre-v13 unpriced fallback when pricingKnown is omitted", () => {
    useRequestDetailMock.mockReturnValue({
      data: {
        requestId: "legacy-shape-unpriced",
        providerId: "nexus",
        providerName: "Nexus",
        appType: "codex",
        model: "custom-model",
        tokenUsageKnown: true,
        costMultiplier: "1",
        inputTokens: 100,
        outputTokens: 20,
        cacheReadTokens: 0,
        cacheCreationTokens: 0,
        inputCostUsd: "0",
        outputCostUsd: "0",
        cacheReadCostUsd: "0",
        cacheCreationCostUsd: "0",
        totalCostUsd: "0",
        isStreaming: false,
        latencyMs: 500,
        statusCode: 200,
        createdAt: 1_725_555_120,
        dataSource: "proxy",
      },
      isLoading: false,
      isError: false,
      refetch: vi.fn(),
    });

    render(
      <RequestDetailPanel
        requestId="legacy-shape-unpriced"
        onClose={vi.fn()}
      />,
    );

    expect(
      screen.getByText(/no configured pricing rule matched/),
    ).toBeVisible();
    expect(
      screen.queryByText(/Pricing is unknown for this legacy request/),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("$0.000000")).not.toBeInTheDocument();
    const pricingModel = screen.getByTitle(
      "The pricing model was not recorded for this request.",
    );
    expect(pricingModel).toHaveTextContent("N/A");
    expect(pricingModel).not.toHaveTextContent("custom-model");
  });
});

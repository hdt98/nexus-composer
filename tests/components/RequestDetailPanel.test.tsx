import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { RequestDetailPanel } from "@/components/usage/RequestDetailPanel";
import viLocale from "@/i18n/locales/vi.json";
import type { RequestLog } from "@/types/usage";

const useRequestDetailMock = vi.hoisted(() => vi.fn());
const copyTextMock = vi.hoisted(() => vi.fn());

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, fallback?: string) => fallback ?? key,
    i18n: { language: "en" },
  }),
}));

vi.mock("@/lib/query/usage", () => ({
  useRequestDetail: (requestId: string) => useRequestDetailMock(requestId),
}));

vi.mock("@/lib/clipboard", () => ({
  copyText: copyTextMock,
}));

vi.mock("@/components/ui/dialog", () => ({
  Dialog: ({ children }: any) => <div>{children}</div>,
  DialogContent: ({ children }: any) => <div>{children}</div>,
  DialogHeader: ({ children }: any) => <div>{children}</div>,
  DialogTitle: ({ children }: any) => <h2>{children}</h2>,
}));

vi.mock("@/components/ui/button", () => ({
  Button: ({ children, ...props }: any) => (
    <button {...props}>{children}</button>
  ),
}));

describe("RequestDetailPanel", () => {
  const request: RequestLog = {
    requestId: "response-id",
    correlationId: "server-request-id",
    providerId: "provider-1",
    providerName: "Nexus",
    appType: "codex",
    model: "glm-5.2",
    costMultiplier: "1",
    inputTokens: 10,
    outputTokens: 2,
    cacheReadTokens: 0,
    cacheCreationTokens: 0,
    inputCostUsd: "0",
    outputCostUsd: "0",
    cacheReadCostUsd: "0",
    cacheCreationCostUsd: "0",
    totalCostUsd: "0",
    isStreaming: true,
    latencyMs: 100,
    statusCode: 200,
    createdAt: 1_710_000_000,
    dataSource: "proxy",
  };

  beforeEach(() => {
    useRequestDetailMock.mockReturnValue({ data: request, isLoading: false });
    copyTextMock.mockReset();
    copyTextMock.mockResolvedValue(undefined);
  });

  it("shows and copies the server correlation ID", () => {
    render(<RequestDetailPanel requestId="response-id" onClose={vi.fn()} />);

    expect(screen.getByText("server-request-id")).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: "Copy server request ID" }),
    );
    expect(copyTextMock).toHaveBeenCalledWith("server-request-id");
  });

  it("uses concise Vietnamese request-detail labels", () => {
    expect({
      title: viLocale.usage.requestDetail,
      general: viLocale.usage.basicInfo,
      tokens: viLocale.usage.tokenUsage,
      cost: viLocale.usage.costBreakdown,
      performance: viLocale.usage.performance,
      nexusId: viLocale.usage.requestId,
      serverId: viLocale.usage.correlationId,
      copy: viLocale.usage.copyCorrelationId,
    }).toEqual({
      title: "Chi tiết yêu cầu",
      general: "Thông tin chung",
      tokens: "Token",
      cost: "Chi phí",
      performance: "Hiệu năng",
      nexusId: "Mã trong Nexus",
      serverId: "Mã yêu cầu",
      copy: "Sao chép",
    });
  });

  it.each([
    ["loaded", { data: request, isLoading: false }],
    ["loading", { data: undefined, isLoading: true }],
    ["not found", { data: undefined, isLoading: false }],
  ])("provides a visible close action when %s", (_state, result) => {
    useRequestDetailMock.mockReturnValue(result);
    const onClose = vi.fn();

    render(<RequestDetailPanel requestId="response-id" onClose={onClose} />);

    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(onClose).toHaveBeenCalledOnce();
  });
});

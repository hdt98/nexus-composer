import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { RequestLogTable } from "@/components/usage/RequestLogTable";
import type { RequestLog, UsageRangeSelection } from "@/types/usage";

const useRequestLogsMock = vi.hoisted(() => vi.fn());
const copyTextMock = vi.hoisted(() => vi.fn());

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (
      key: string,
      options?: {
        defaultValue?: string;
      },
    ) => options?.defaultValue ?? key,
    i18n: {
      resolvedLanguage: "en",
      language: "en",
    },
  }),
}));

vi.mock("@/lib/query/usage", () => ({
  useRequestLogs: (args: unknown) => useRequestLogsMock(args),
}));

vi.mock("@/lib/clipboard", () => ({
  copyText: copyTextMock,
}));

vi.mock("@/components/ui/button", () => ({
  Button: ({ children, ...props }: any) => (
    <button {...props}>{children}</button>
  ),
}));

vi.mock("@/components/ui/input", () => ({
  Input: (props: any) => <input {...props} />,
}));

vi.mock("@/components/ui/select", () => ({
  Select: ({ children }: any) => <div>{children}</div>,
  SelectTrigger: ({ children, ...props }: any) => (
    <button type="button" {...props}>
      {children}
    </button>
  ),
  SelectValue: ({ placeholder }: any) => <span>{placeholder ?? null}</span>,
  SelectContent: () => null,
  SelectItem: () => null,
}));

vi.mock("@/components/ui/table", () => ({
  Table: ({ children }: any) => <table>{children}</table>,
  TableBody: ({ children }: any) => <tbody>{children}</tbody>,
  TableCell: ({ children, ...props }: any) => <td {...props}>{children}</td>,
  TableHead: ({ children, ...props }: any) => <th {...props}>{children}</th>,
  TableHeader: ({ children }: any) => <thead>{children}</thead>,
  TableRow: ({ children }: any) => <tr>{children}</tr>,
}));

vi.mock("@/components/usage/RequestDetailPanel", () => ({
  RequestDetailPanel: ({ requestId }: { requestId: string }) => (
    <div>detail:{requestId}</div>
  ),
}));

const requestLog: RequestLog = {
  requestId: "response-id",
  correlationId: "667da1ce-c07f-49fd-ad32-3e2fc009da6c",
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

function mockSingleLog() {
  useRequestLogsMock.mockReturnValue({
    data: { data: [requestLog], total: 1, page: 0, pageSize: 20 },
    isLoading: false,
  });
}

describe("RequestLogTable", () => {
  beforeEach(() => {
    useRequestLogsMock.mockReset();
    copyTextMock.mockReset();
    copyTextMock.mockResolvedValue(undefined);
    useRequestLogsMock.mockImplementation(
      ({ page = 0, pageSize = 20 }: { page?: number; pageSize?: number }) => ({
        data: {
          data: [],
          total: 120,
          page,
          pageSize,
        },
        isLoading: false,
      }),
    );
  });

  it("resets pagination when the dashboard range changes", async () => {
    const initialRange: UsageRangeSelection = { preset: "today" };
    const nextRange: UsageRangeSelection = {
      preset: "custom",
      customStartDate: 1_710_000_000,
      customEndDate: 1_710_086_400,
    };

    const { rerender } = render(
      <RequestLogTable
        range={initialRange}
        rangeLabel="Today"
        appType="all"
        refreshIntervalMs={0}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "2" }));

    await waitFor(() => {
      expect(useRequestLogsMock).toHaveBeenLastCalledWith(
        expect.objectContaining({
          page: 1,
          range: initialRange,
        }),
      );
    });

    rerender(
      <RequestLogTable
        range={nextRange}
        rangeLabel="Custom"
        appType="all"
        refreshIntervalMs={0}
      />,
    );

    await waitFor(() => {
      expect(useRequestLogsMock).toHaveBeenLastCalledWith(
        expect.objectContaining({
          page: 0,
          range: nextRange,
        }),
      );
    });
  });

  it("resets pagination when the dashboard app filter changes", async () => {
    const range: UsageRangeSelection = { preset: "today" };
    const { rerender } = render(
      <RequestLogTable
        range={range}
        rangeLabel="Today"
        appType="all"
        refreshIntervalMs={0}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "2" }));

    await waitFor(() => {
      expect(useRequestLogsMock).toHaveBeenLastCalledWith(
        expect.objectContaining({
          page: 1,
          range,
        }),
      );
    });

    rerender(
      <RequestLogTable
        range={range}
        rangeLabel="Today"
        appType="claude"
        refreshIntervalMs={0}
      />,
    );

    await waitFor(() => {
      expect(useRequestLogsMock).toHaveBeenLastCalledWith(
        expect.objectContaining({
          page: 0,
          range,
        }),
      );
    });
  });

  it("opens request details from a log row", () => {
    mockSingleLog();

    render(
      <RequestLogTable
        range={{ preset: "today" }}
        rangeLabel="Today"
        appType="all"
        refreshIntervalMs={0}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "common.view" }));
    expect(screen.getByText("detail:response-id")).toBeInTheDocument();
  });

  it("shows and copies the server request ID from the log row", () => {
    mockSingleLog();

    render(
      <RequestLogTable
        range={{ preset: "today" }}
        rangeLabel="Today"
        appType="all"
        refreshIntervalMs={0}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", { name: "usage.copyCorrelationId" }),
    );
    expect(screen.getByText("667da1ce…")).toBeInTheDocument();
    expect(copyTextMock).toHaveBeenCalledWith(requestLog.correlationId);
  });
});

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { SessionSyncButton } from "@/components/usage/SessionSyncButton";
import viLocale from "@/i18n/locales/vi.json";

const mocks = vi.hoisted(() => ({
  invalidateQueries: vi.fn(),
  syncSessionUsage: vi.fn(),
  toastError: vi.fn(),
  toastInfo: vi.fn(),
  toastSuccess: vi.fn(),
  toastWarning: vi.fn(),
}));

vi.mock("@tanstack/react-query", () => ({
  useQueryClient: () => ({ invalidateQueries: mocks.invalidateQueries }),
}));

vi.mock("@/lib/api/usage", () => ({
  usageApi: { syncSessionUsage: mocks.syncSessionUsage },
}));

vi.mock("sonner", () => ({
  toast: {
    error: mocks.toastError,
    info: mocks.toastInfo,
    success: mocks.toastSuccess,
    warning: mocks.toastWarning,
  },
}));

vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string, values?: Record<string, number | string>) => {
      if (key === "usage.sessionSync.imported") {
        return `Imported ${values?.count} records`;
      }
      if (key === "usage.sessionSync.partial") {
        return `Records imported: ${values?.imported}; errors: ${values?.failed}`;
      }
      if (key === "usage.sessionSync.failedWithCount") {
        return `Session sync failed; errors: ${values?.count}`;
      }
      return (
        {
          "usage.sessionSync.trigger": "Sync session logs",
          "usage.sessionSync.resync": "Sync",
          "usage.sessionSync.upToDate": "Session logs are up to date",
          "usage.sessionSync.failed": "Session sync failed",
        }[key] ?? key
      );
    },
  }),
}));

const syncResult = (imported: number, errors: string[]) => ({
  imported,
  skipped: 0,
  filesScanned: 1,
  errors,
});

function startSync() {
  render(<SessionSyncButton />);
  const button = screen.getByRole("button", { name: "Sync session logs" });
  fireEvent.click(button);
  return button;
}

describe("SessionSyncButton", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("has concise Vietnamese copy for every mounted sync state", () => {
    expect(viLocale.usage.sessionSync).toEqual({
      trigger: "Đồng bộ nhật ký phiên",
      import: "Nhập nhật ký phiên",
      resync: "Đồng bộ",
      imported: "Đã nhập {{count}} mục",
      partial: "Đã nhập {{imported}} mục; {{failed}} lỗi",
      upToDate: "Dữ liệu đã được cập nhật",
      failed: "Không thể đồng bộ",
      failedWithCount: "Không thể đồng bộ: {{count}} lỗi",
    });
  });

  it("reports session logs as up to date when nothing changed", async () => {
    mocks.syncSessionUsage.mockResolvedValue(syncResult(0, []));

    startSync();

    await waitFor(() =>
      expect(mocks.toastInfo).toHaveBeenCalledWith(
        "Session logs are up to date",
      ),
    );
    expect(mocks.invalidateQueries).not.toHaveBeenCalled();
  });

  it("reports imports and refreshes usage data", async () => {
    mocks.syncSessionUsage.mockResolvedValue(syncResult(2, []));

    startSync();

    await waitFor(() =>
      expect(mocks.toastSuccess).toHaveBeenCalledWith("Imported 2 records"),
    );
    expect(mocks.invalidateQueries).toHaveBeenCalledWith({
      queryKey: ["usage"],
    });
  });

  it("reports partial imports and refreshes usage data", async () => {
    mocks.syncSessionUsage.mockResolvedValue(syncResult(3, ["one failure"]));

    startSync();

    await waitFor(() =>
      expect(mocks.toastWarning).toHaveBeenCalledWith(
        "Records imported: 3; errors: 1",
      ),
    );
    expect(mocks.invalidateQueries).toHaveBeenCalledWith({
      queryKey: ["usage"],
    });
  });

  it("reports a returned full failure", async () => {
    mocks.syncSessionUsage.mockResolvedValue(
      syncResult(0, ["first", "second"]),
    );

    startSync();

    await waitFor(() =>
      expect(mocks.toastError).toHaveBeenCalledWith(
        "Session sync failed; errors: 2",
      ),
    );
    expect(mocks.invalidateQueries).not.toHaveBeenCalled();
  });

  it("reports a rejected sync", async () => {
    mocks.syncSessionUsage.mockRejectedValue(new Error("invoke failed"));

    startSync();

    await waitFor(() =>
      expect(mocks.toastError).toHaveBeenCalledWith("Session sync failed"),
    );
    expect(mocks.invalidateQueries).not.toHaveBeenCalled();
  });

  it("disables while syncing and resets after completion", async () => {
    let resolveSync: ((value: unknown) => void) | undefined;
    mocks.syncSessionUsage.mockReturnValue(
      new Promise((resolve) => {
        resolveSync = resolve;
      }),
    );

    const button = startSync();
    expect(button).toBeDisabled();

    resolveSync?.(syncResult(0, []));

    await waitFor(() => expect(button).not.toBeDisabled());
  });
});

import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ConfirmDialog } from "@/components/ConfirmDialog";

describe("ConfirmDialog", () => {
  it("shows pending feedback and prevents duplicate actions", () => {
    const onConfirm = vi.fn();
    const onCancel = vi.fn();

    render(
      <ConfirmDialog
        isOpen
        title="Overwrite remote config?"
        message="Existing files will be backed up."
        confirmText="Confirm switch"
        cancelText="Cancel"
        isLoading
        loadingText="Switching remote provider..."
        onConfirm={onConfirm}
        onCancel={onCancel}
      />,
    );

    const pendingButton = screen.getByRole("button", {
      name: "Switching remote provider...",
    });
    const cancelButton = screen.getByRole("button", { name: "Cancel" });

    expect(pendingButton).toBeDisabled();
    expect(pendingButton).toHaveAttribute("aria-busy", "true");
    expect(cancelButton).toBeDisabled();

    fireEvent.click(pendingButton);
    fireEvent.click(cancelButton);
    expect(onConfirm).not.toHaveBeenCalled();
    expect(onCancel).not.toHaveBeenCalled();
  });

  it("confirms normally before the operation starts", () => {
    const onConfirm = vi.fn();

    render(
      <ConfirmDialog
        isOpen
        title="Overwrite remote config?"
        message="Existing files will be backed up."
        confirmText="Confirm switch"
        onConfirm={onConfirm}
        onCancel={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Confirm switch" }));
    expect(onConfirm).toHaveBeenCalledWith(false);
  });
});

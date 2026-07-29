import React from "react";
import { createPortal } from "react-dom";
import { motion, AnimatePresence } from "framer-motion";
import { ArrowLeft } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  isWindows,
  isLinux,
  DRAG_REGION_ATTR,
  DRAG_REGION_STYLE,
} from "@/lib/platform";
import { isTextEditableTarget } from "@/utils/domUtils";
import { cn } from "@/lib/utils";

interface FullScreenPanelProps {
  isOpen: boolean;
  title: string;
  onClose: () => void;
  children: React.ReactNode;
  footer?: React.ReactNode;
  /**
   * Overrides padding and spacing classes on the content scroll container.
   * Defaults to `px-6 py-6 space-y-6`. Classes merge through `cn` (twMerge), so
   * a value such as `pt-3` overrides only top padding while retaining defaults.
   */
  contentClassName?: string;
}

const DRAG_BAR_HEIGHT = isWindows() || isLinux() ? 0 : 28; // px - match App.tsx
const HEADER_HEIGHT = 64; // px - match App.tsx

/**
 * Reusable full-screen panel component
 * Handles portal rendering, header with back button, and footer
 * Uses solid theme colors without transparency
 */
export const FullScreenPanel: React.FC<FullScreenPanelProps> = ({
  isOpen,
  title,
  onClose,
  children,
  footer,
  contentClassName,
}) => {
  React.useEffect(() => {
    if (isOpen) {
      document.body.style.overflow = "hidden";
    }
    return () => {
      document.body.style.overflow = "";
    };
  }, [isOpen]);

  // Close the panel with Escape.
  const onCloseRef = React.useRef(onClose);

  React.useEffect(() => {
    onCloseRef.current = onClose;
  }, [onClose]);

  React.useEffect(() => {
    if (!isOpen) return;

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        // Do not close the panel if a child such as a Radix Select, Dialog, or
        // Dropdown has already handled Escape.
        if (event.defaultPrevented) {
          return;
        }

        if (isTextEditableTarget(event.target)) {
          return; // Let editable fields handle Escape, such as clearing or blurring.
        }

        event.stopPropagation(); // Avoid triggering the global listener in App.tsx.
        onCloseRef.current();
      }
    };

    // Listen during bubbling so child components such as Radix UI handle Escape first.
    window.addEventListener("keydown", handleKeyDown, false);
    return () => {
      window.removeEventListener("keydown", handleKeyDown, false);
    };
  }, [isOpen]);

  return createPortal(
    <AnimatePresence>
      {isOpen && (
        <motion.div
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: 0.2 }}
          className="fixed inset-0 z-[60] flex flex-col"
          style={{ backgroundColor: "hsl(var(--background))" }}
        >
          {/* Drag region matching App.tsx. Linux uses DRAG_BAR_HEIGHT=0 and
              omits the element; macOS retains a 28px draggable spacer. */}
          {DRAG_BAR_HEIGHT > 0 && (
            <div
              data-tauri-drag-region
              style={
                {
                  WebkitAppRegion: "drag",
                  height: DRAG_BAR_HEIGHT,
                } as React.CSSProperties
              }
            />
          )}

          {/* Header - match App.tsx */}
          <div
            className="flex-shrink-0 flex items-center"
            {...DRAG_REGION_ATTR}
            style={
              {
                ...DRAG_REGION_STYLE,
                backgroundColor: "hsl(var(--background))",
                height: HEADER_HEIGHT,
              } as React.CSSProperties
            }
          >
            <div
              className="px-6 w-full flex items-center gap-4"
              {...DRAG_REGION_ATTR}
              style={{ ...DRAG_REGION_STYLE } as React.CSSProperties}
            >
              <Button
                type="button"
                variant="outline"
                size="icon"
                onClick={onClose}
                className="rounded-lg select-none"
                style={{ WebkitAppRegion: "no-drag" } as React.CSSProperties}
              >
                <ArrowLeft className="h-4 w-4" />
              </Button>
              <h2 className="text-lg font-semibold text-foreground select-none">
                {title}
              </h2>
            </div>
          </div>

          {/* Content */}
          <div className="flex-1 overflow-y-auto scroll-overlay">
            <div className={cn("px-6 py-6 space-y-6 w-full", contentClassName)}>
              {children}
            </div>
          </div>

          {/* Footer */}
          {footer && (
            <div
              className="flex-shrink-0 py-4 border-t border-border-default"
              style={{ backgroundColor: "hsl(var(--background))" }}
            >
              <div className="px-6 flex items-center justify-end gap-3">
                {footer}
              </div>
            </div>
          )}
        </motion.div>
      )}
    </AnimatePresence>,
    document.body,
  );
};

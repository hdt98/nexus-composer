import { cn } from "@/lib/utils";
import { useTranslation } from "react-i18next";

interface FailoverPriorityBadgeProps {
  priority: number; // 1, 2, 3, ...
  className?: string;
}

/**
 * Badge showing a provider's priority in the failover queue.
 */
export function FailoverPriorityBadge({
  priority,
  className,
}: FailoverPriorityBadgeProps) {
  const { t } = useTranslation();

  return (
    <div
      className={cn(
        "inline-flex items-center px-1.5 py-0.5 rounded text-xs font-semibold",
        "bg-emerald-500/10 text-emerald-600 dark:text-emerald-400",
        className,
      )}
      title={t("failover.priority.tooltip", {
        priority,
        defaultValue: "Failover priority {{priority}}",
      })}
    >
      P{priority}
    </div>
  );
}

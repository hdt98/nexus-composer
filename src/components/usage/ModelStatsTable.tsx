import { useTranslation } from "react-i18next";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { useModelStats } from "@/lib/query/usage";
import { fmtUsd } from "./format";
import type { UsageRangeSelection, UsageSourceFilter } from "@/types/usage";

interface ModelStatsTableProps {
  range: UsageRangeSelection;
  appType?: string;
  source?: UsageSourceFilter;
  refreshIntervalMs: number;
}

export function ModelStatsTable({
  range,
  appType,
  source,
  refreshIntervalMs,
}: ModelStatsTableProps) {
  const { t } = useTranslation();
  const { data: stats, isLoading } = useModelStats(range, appType, source, {
    refetchInterval: refreshIntervalMs > 0 ? refreshIntervalMs : false,
  });

  if (isLoading) {
    return <div className="h-[400px] animate-pulse rounded bg-gray-100" />;
  }

  return (
    <div className="rounded-lg border border-border/50 bg-card/40 backdrop-blur-sm overflow-hidden">
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>{t("usage.model", "Model")}</TableHead>
            <TableHead className="text-right">
              {t("usage.requests", "Requests")}
            </TableHead>
            <TableHead className="text-right">
              {t("usage.tokens", "Tokens")}
            </TableHead>
            <TableHead className="text-right">
              {t("usage.totalCost", "Total Cost")}
            </TableHead>
            <TableHead className="text-right">
              {t("usage.avgCost", "Average Cost")}
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {stats?.length === 0 ? (
            <TableRow>
              <TableCell
                colSpan={5}
                className="text-center text-muted-foreground"
              >
                {t("usage.noData", "No data")}
              </TableCell>
            </TableRow>
          ) : (
            stats?.map((stat) => (
              <TableRow key={stat.model}>
                <TableCell className="font-mono text-sm">
                  {stat.model}
                </TableCell>
                <TableCell className="text-right">
                  {stat.requestCount.toLocaleString()}
                </TableCell>
                <TableCell className="text-right">
                  {stat.totalTokens.toLocaleString()}
                </TableCell>
                <TableCell className="text-right">
                  {fmtUsd(stat.totalCost, 4)}
                </TableCell>
                <TableCell className="text-right">
                  {fmtUsd(stat.avgCostPerRequest, 6)}
                </TableCell>
              </TableRow>
            ))
          )}
        </TableBody>
      </Table>
    </div>
  );
}

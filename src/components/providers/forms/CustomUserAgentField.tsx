import { useTranslation } from "react-i18next";
import { ChevronDown } from "lucide-react";
import { FormLabel } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { isValidUserAgentHeader } from "@/lib/userAgent";
import { USER_AGENT_PRESETS } from "@/config/userAgentPresets";

interface CustomUserAgentFieldProps {
  /** Input ID used by label htmlFor; each form must provide a unique value. */
  id: string;
  value: string;
  onChange: (value: string) => void;
}

/**
 * Provider-level custom User-Agent field shared by Claude and Codex forms.
 *
 * Includes a label, input, preset menu, and live validation. Validation matches
 * backend `parse_custom_user_agent`; see `@/lib/userAgent`. Invalid input shows
 * a non-blocking error and remains silently ignored at runtime.
 */
export function CustomUserAgentField({
  id,
  value,
  onChange,
}: CustomUserAgentFieldProps) {
  const { t } = useTranslation();
  const valid = isValidUserAgentHeader(value);

  return (
    <div className="space-y-2">
      <FormLabel htmlFor={id}>
        {t("providerForm.customUserAgent", {
          defaultValue: "Custom User-Agent",
        })}
      </FormLabel>
      <div className="flex items-center gap-2">
        <Input
          id={id}
          type="text"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder="Mozilla/5.0 ..."
          autoComplete="off"
          className="flex-1"
        />
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button type="button" variant="outline" className="shrink-0 gap-1">
              {t("providerForm.customUserAgentPresets", {
                defaultValue: "Presets",
              })}
              <ChevronDown className="h-3.5 w-3.5 opacity-60" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent
            align="end"
            className="max-h-64 overflow-y-auto z-[200]"
          >
            {USER_AGENT_PRESETS.map((preset) => (
              <DropdownMenuItem
                key={preset}
                onSelect={() => onChange(preset)}
                className="font-mono text-xs"
              >
                {preset}
              </DropdownMenuItem>
            ))}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      {valid ? (
        <p className="text-xs text-muted-foreground">
          {t("providerForm.customUserAgentHint", {
            defaultValue:
              "Only takes effect when local routing/proxy takeover is enabled; replaces the User-Agent in requests forwarded to the provider API.",
          })}
        </p>
      ) : (
        <p className="text-xs text-destructive">
          {t("providerForm.customUserAgentInvalid", {
            defaultValue:
              "User-Agent must not contain control characters (e.g. line breaks); otherwise it will be ignored.",
          })}
        </p>
      )}
    </div>
  );
}

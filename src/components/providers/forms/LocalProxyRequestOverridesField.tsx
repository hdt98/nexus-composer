import { useTranslation } from "react-i18next";
import { FormLabel } from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import {
  formatRequestOverrideObject,
  hasInvalidMaxOutputTokens,
  isPositiveSafeInteger,
  parseBodyOverrideJson,
  parseHeaderOverrideJson,
} from "@/lib/requestOverrides";

interface LocalProxyMaxOutputTokensFieldProps {
  bodyJson: string;
  onBodyJsonChange: (value: string) => void;
  showClaudeDesktopTimeoutWarning?: boolean;
}

export function LocalProxyMaxOutputTokensField({
  bodyJson,
  onBodyJsonChange,
  showClaudeDesktopTimeoutWarning = false,
}: LocalProxyMaxOutputTokensFieldProps) {
  const { t } = useTranslation();
  const parsedBody = parseBodyOverrideJson(bodyJson);
  const configuredValue = parsedBody.value?.max_tokens;
  const canEditBody = !parsedBody.error || Boolean(parsedBody.value);
  const maxOutputTokensInvalid = hasInvalidMaxOutputTokens(parsedBody.value);

  const handleChange = (rawValue: string) => {
    if (!canEditBody) return;
    const body = { ...(parsedBody.value ?? {}) };
    if (!rawValue.trim()) {
      delete body.max_tokens;
    } else {
      const value = Number(rawValue);
      body.max_tokens = isPositiveSafeInteger(value) ? value : rawValue;
    }
    onBodyJsonChange(formatRequestOverrideObject(body));
  };

  const inputValue =
    configuredValue === undefined || configuredValue === null
      ? ""
      : typeof configuredValue === "object"
        ? JSON.stringify(configuredValue)
        : String(configuredValue);

  return (
    <div className="space-y-2">
      <FormLabel htmlFor="local-proxy-max-output-tokens">
        {t("providerForm.maxOutputTokens")}
      </FormLabel>
      <Input
        id="local-proxy-max-output-tokens"
        type="text"
        inputMode="numeric"
        value={inputValue}
        onChange={(event) => handleChange(event.target.value)}
        placeholder="65536"
        disabled={!canEditBody}
        aria-invalid={Boolean(parsedBody.error || maxOutputTokensInvalid)}
      />
      <p className="text-xs text-muted-foreground">
        {t("providerForm.maxOutputTokensHint")}
      </p>
      {showClaudeDesktopTimeoutWarning && (
        <p className="text-xs text-amber-600 dark:text-amber-400">
          {t("providerForm.claudeDesktopMaxOutputTokensWarning")}
        </p>
      )}
      {(parsedBody.error || maxOutputTokensInvalid) && (
        <p className="text-xs text-destructive">
          {maxOutputTokensInvalid
            ? t("providerForm.maxOutputTokensInvalid")
            : t("providerForm.localProxyBodyOverridesInvalidDetail", {
                error: parsedBody.error,
              })}
        </p>
      )}
    </div>
  );
}

interface LocalProxyRequestOverridesFieldProps {
  headersJson: string;
  bodyJson: string;
  onHeadersJsonChange: (value: string) => void;
  onBodyJsonChange: (value: string) => void;
}

export function LocalProxyRequestOverridesField({
  headersJson,
  bodyJson,
  onHeadersJsonChange,
  onBodyJsonChange,
}: LocalProxyRequestOverridesFieldProps) {
  const { t } = useTranslation();
  const headerError = parseHeaderOverrideJson(headersJson).error;
  const bodyError = parseBodyOverrideJson(bodyJson).error;

  return (
    <div className="space-y-3">
      <div className="space-y-1">
        <FormLabel>
          {t("providerForm.localProxyRequestOverrides", {
            defaultValue: "本地代理请求覆盖",
          })}
        </FormLabel>
        <p className="text-xs text-muted-foreground">
          {t("providerForm.localProxyRequestOverridesHint", {
            defaultValue:
              "仅在本地路由/代理接管后生效，应用于协议转换后的上游请求。",
          })}
        </p>
      </div>

      <div className="grid gap-3 md:grid-cols-2">
        <div className="space-y-2">
          <FormLabel className="text-xs text-muted-foreground">
            {t("providerForm.localProxyHeaderOverrides", {
              defaultValue: "Header 覆盖",
            })}
          </FormLabel>
          <Textarea
            value={headersJson}
            onChange={(event) => onHeadersJsonChange(event.target.value)}
            placeholder={'{\n  "X-Provider": "nexus-composer"\n}'}
            className="min-h-[132px] resize-y font-mono text-xs"
            aria-invalid={Boolean(headerError)}
          />
          {headerError && (
            <p className="text-xs text-destructive">
              {t("providerForm.localProxyHeaderOverridesInvalidDetail", {
                error: headerError,
                defaultValue: "Header 覆盖格式错误：{{error}}",
              })}
            </p>
          )}
        </div>

        <div className="space-y-2">
          <FormLabel className="text-xs text-muted-foreground">
            {t("providerForm.localProxyBodyOverrides", {
              defaultValue: "Body 覆盖",
            })}
          </FormLabel>
          <Textarea
            value={bodyJson}
            onChange={(event) => onBodyJsonChange(event.target.value)}
            placeholder={'{\n  "temperature": 0.2\n}'}
            className="min-h-[132px] resize-y font-mono text-xs"
            aria-invalid={Boolean(bodyError)}
          />
          {bodyError && (
            <p className="text-xs text-destructive">
              {t("providerForm.localProxyBodyOverridesInvalidDetail", {
                error: bodyError,
                defaultValue: "Body 覆盖格式错误：{{error}}",
              })}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

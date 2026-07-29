import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Loader2,
  LogOut,
  Copy,
  Check,
  ExternalLink,
  Plus,
  X,
  Sparkles,
  User,
} from "lucide-react";
import { useCodexOauth } from "./hooks/useCodexOauth";
import { copyText } from "@/lib/clipboard";

interface CodexOAuthSectionProps {
  className?: string;
  /** Currently selected ChatGPT account ID. */
  selectedAccountId?: string | null;
  /** Called when the selected account changes. */
  onAccountSelect?: (accountId: string | null) => void;
  /** Whether Codex FAST mode is enabled. */
  fastModeEnabled?: boolean;
  /** Called when FAST mode changes. */
  onFastModeChange?: (enabled: boolean) => void;
}

/**
 * Codex OAuth authentication section.
 *
 * Signs in to a ChatGPT Plus or Pro account through the OpenAI device-code
 * flow so Claude Code requests can be proxied to the Codex backend API.
 */
export const CodexOAuthSection: React.FC<CodexOAuthSectionProps> = ({
  className,
  selectedAccountId,
  onAccountSelect,
  fastModeEnabled = false,
  onFastModeChange,
}) => {
  const { t } = useTranslation();
  const [copied, setCopied] = React.useState(false);

  const {
    accounts,
    defaultAccountId,
    hasAnyAccount,
    pollingState,
    deviceCode,
    error,
    isPolling,
    isAddingAccount,
    isRemovingAccount,
    isSettingDefaultAccount,
    addAccount,
    removeAccount,
    setDefaultAccount,
    cancelAuth,
    logout,
  } = useCodexOauth();

  const copyUserCode = async () => {
    if (deviceCode?.user_code) {
      await copyText(deviceCode.user_code);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  const handleAccountSelect = (value: string) => {
    onAccountSelect?.(value === "none" ? null : value);
  };

  const handleRemoveAccount = (accountId: string, e: React.MouseEvent) => {
    e.stopPropagation();
    e.preventDefault();
    removeAccount(accountId);
    if (selectedAccountId === accountId) {
      onAccountSelect?.(null);
    }
  };

  return (
    <div className={`space-y-4 ${className || ""}`}>
      {/* Authentication status */}
      <div className="flex items-center justify-between">
        <Label>{t("codexOauth.authStatus", "Authentication status")}</Label>
        <Badge
          variant={hasAnyAccount ? "default" : "secondary"}
          className={hasAnyAccount ? "bg-green-500 hover:bg-green-600" : ""}
        >
          {hasAnyAccount
            ? t("codexOauth.accountCount", {
                count: accounts.length,
                defaultValue: `${accounts.length} accounts`,
              })
            : t("codexOauth.notAuthenticated", "Not authenticated")}
        </Badge>
      </div>

      {/* Account selector */}
      {hasAnyAccount && onAccountSelect && (
        <div className="space-y-2">
          <Label className="text-sm text-muted-foreground">
            {t("codexOauth.selectAccount", "Select account")}
          </Label>
          <Select
            value={selectedAccountId || "none"}
            onValueChange={handleAccountSelect}
          >
            <SelectTrigger>
              <SelectValue
                placeholder={t(
                  "codexOauth.selectAccountPlaceholder",
                  "Select a ChatGPT account",
                )}
              />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="none">
                <span className="text-muted-foreground">
                  {t("codexOauth.useDefaultAccount", "Use default account")}
                </span>
              </SelectItem>
              {accounts.map((account) => (
                <SelectItem key={account.id} value={account.id}>
                  <div className="flex items-center gap-2">
                    <User className="h-4 w-4 text-muted-foreground" />
                    <span>{account.login}</span>
                  </div>
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      )}

      {onFastModeChange && (
        <div className="flex items-center justify-between rounded-md border bg-muted/30 p-3">
          <div className="space-y-1 pr-4">
            <Label className="text-sm font-medium">
              {t("codexOauth.fastMode", "FAST mode")}
            </Label>
            <p className="text-xs text-muted-foreground">
              {t("codexOauth.fastModeDescription", {
                defaultValue:
                  'Send service_tier="priority" for lower latency. Turn it off if the ChatGPT Codex backend rejects the parameter.',
              })}
            </p>
          </div>
          <Switch
            checked={fastModeEnabled}
            onCheckedChange={onFastModeChange}
            aria-label={t("codexOauth.fastMode", "FAST mode")}
          />
        </div>
      )}

      {/* Signed-in accounts */}
      {hasAnyAccount && (
        <div className="space-y-2">
          <Label className="text-sm text-muted-foreground">
            {t("codexOauth.loggedInAccounts", "Signed-in accounts")}
          </Label>
          <div className="space-y-1">
            {accounts.map((account) => (
              <div
                key={account.id}
                className="flex items-center justify-between p-2 rounded-md border bg-muted/30"
              >
                <div className="flex items-center gap-2">
                  <User className="h-5 w-5 text-muted-foreground" />
                  <span className="text-sm font-medium">{account.login}</span>
                  {defaultAccountId === account.id && (
                    <Badge variant="secondary" className="text-xs">
                      {t("codexOauth.defaultAccount", "Default")}
                    </Badge>
                  )}
                  {selectedAccountId === account.id && (
                    <Badge variant="outline" className="text-xs">
                      {t("codexOauth.selected", "Selected")}
                    </Badge>
                  )}
                </div>
                <div className="flex items-center gap-1">
                  {defaultAccountId !== account.id && (
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      className="h-7 px-2 text-xs text-muted-foreground"
                      onClick={() => setDefaultAccount(account.id)}
                      disabled={isSettingDefaultAccount}
                    >
                      {t("codexOauth.setAsDefault", "Set as default")}
                    </Button>
                  )}
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    className="h-7 w-7 text-muted-foreground hover:text-red-500"
                    onClick={(e) => handleRemoveAccount(account.id, e)}
                    disabled={isRemovingAccount}
                    title={t("codexOauth.removeAccount", "Remove account")}
                  >
                    <X className="h-4 w-4" />
                  </Button>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}

      {/* Sign-in button when no account is authenticated */}
      {!hasAnyAccount && pollingState === "idle" && (
        <Button
          type="button"
          onClick={addAccount}
          className="w-full"
          variant="outline"
        >
          <Sparkles className="mr-2 h-4 w-4" />
          {t("codexOauth.loginWithChatGPT", "Sign in with ChatGPT")}
        </Button>
      )}

      {/* Add another account */}
      {hasAnyAccount && pollingState === "idle" && (
        <Button
          type="button"
          onClick={addAccount}
          className="w-full"
          variant="outline"
          disabled={isAddingAccount}
        >
          <Plus className="mr-2 h-4 w-4" />
          {t("codexOauth.addAnotherAccount", "Add another account")}
        </Button>
      )}

      {/* Authorization polling state */}
      {isPolling && deviceCode && (
        <div className="space-y-3 p-4 rounded-lg border border-border bg-muted/50">
          <div className="flex items-center justify-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            {t("codexOauth.waitingForAuth", "Waiting for authorization...")}
          </div>

          <div className="text-center">
            <p className="text-xs text-muted-foreground mb-1">
              {t("codexOauth.enterCode", "Enter this code in your browser:")}
            </p>
            <div className="flex items-center justify-center gap-2">
              <code className="text-2xl font-mono font-bold tracking-wider bg-background px-4 py-2 rounded border">
                {deviceCode.user_code}
              </code>
              <Button
                type="button"
                size="icon"
                variant="ghost"
                onClick={copyUserCode}
                title={t("codexOauth.copyCode", "Copy code")}
              >
                {copied ? (
                  <Check className="h-4 w-4 text-green-500" />
                ) : (
                  <Copy className="h-4 w-4" />
                )}
              </Button>
            </div>
          </div>

          <div className="text-center">
            <a
              href={deviceCode.verification_uri}
              target="_blank"
              rel="noopener noreferrer"
              className="inline-flex items-center gap-1 text-sm text-blue-500 hover:underline"
            >
              {deviceCode.verification_uri}
              <ExternalLink className="h-3 w-3" />
            </a>
          </div>

          <div className="text-center">
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={cancelAuth}
            >
              {t("common.cancel", "Cancel")}
            </Button>
          </div>
        </div>
      )}

      {/* Error state */}
      {pollingState === "error" && error && (
        <div className="space-y-2">
          <p className="text-sm text-red-500">{error}</p>
          <div className="flex gap-2">
            <Button
              type="button"
              onClick={addAccount}
              variant="outline"
              size="sm"
            >
              {t("codexOauth.retry", "Retry")}
            </Button>
            <Button
              type="button"
              onClick={cancelAuth}
              variant="ghost"
              size="sm"
            >
              {t("common.cancel", "Cancel")}
            </Button>
          </div>
        </div>
      )}

      {/* Sign out of all accounts */}
      {hasAnyAccount && accounts.length > 1 && (
        <Button
          type="button"
          variant="outline"
          onClick={logout}
          className="w-full text-red-500 hover:text-red-600 hover:bg-red-50 dark:hover:bg-red-950"
        >
          <LogOut className="mr-2 h-4 w-4" />
          {t("codexOauth.logoutAll", "Sign out of all accounts")}
        </Button>
      )}
    </div>
  );
};

export default CodexOAuthSection;

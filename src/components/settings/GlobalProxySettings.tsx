/**
 * Global outbound proxy settings.
 *
 * Provides inputs for configuring a global proxy with optional credentials.
 */

import { useState, useEffect, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Loader2, TestTube2, Search, Eye, EyeOff, X } from "lucide-react";
import {
  useGlobalProxyUrl,
  useSetGlobalProxyUrl,
  useTestProxy,
  useScanProxies,
  type DetectedProxy,
} from "@/hooks/useGlobalProxy";

/** Extracts credentials from a complete URL. */
function extractAuth(url: string): {
  baseUrl: string;
  username: string;
  password: string;
} {
  if (!url.trim()) return { baseUrl: "", username: "", password: "" };

  try {
    const parsed = new URL(url);
    const username = decodeURIComponent(parsed.username || "");
    const password = decodeURIComponent(parsed.password || "");
    // Remove credentials to obtain the base URL.
    parsed.username = "";
    parsed.password = "";
    return { baseUrl: parsed.toString(), username, password };
  } catch {
    return { baseUrl: url, username: "", password: "" };
  }
}

/** Merges credentials into a URL. */
function mergeAuth(
  baseUrl: string,
  username: string,
  password: string,
): string {
  if (!baseUrl.trim()) return "";
  if (!username.trim()) return baseUrl;

  try {
    const parsed = new URL(baseUrl);
    // URL username/password setters apply percent-encoding automatically.
    // Do not use encodeURIComponent here because it would double-encode them.
    parsed.username = username.trim();
    if (password) {
      parsed.password = password;
    }
    return parsed.toString();
  } catch {
    // If URL parsing fails, insert manually and encode the credentials here.
    const match = baseUrl.match(/^(\w+:\/\/)(.+)$/);
    if (match) {
      const auth = password
        ? `${encodeURIComponent(username.trim())}:${encodeURIComponent(password)}@`
        : `${encodeURIComponent(username.trim())}@`;
      return `${match[1]}${auth}${match[2]}`;
    }
    return baseUrl;
  }
}

export function GlobalProxySettings() {
  const { t } = useTranslation();
  const { data: savedUrl, isLoading } = useGlobalProxyUrl();
  const setMutation = useSetGlobalProxyUrl();
  const testMutation = useTestProxy();
  const scanMutation = useScanProxies();

  const [url, setUrl] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [showPassword, setShowPassword] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [detected, setDetected] = useState<DetectedProxy[]>([]);

  // Build the complete URL, including credentials.
  const fullUrl = useMemo(
    () => mergeAuth(url, username, password),
    [url, username, password],
  );

  // Synchronize remote configuration.
  useEffect(() => {
    if (savedUrl !== undefined) {
      const { baseUrl, username: u, password: p } = extractAuth(savedUrl || "");
      setUrl(baseUrl);
      setUsername(u);
      setPassword(p);
      setDirty(false);
    }
  }, [savedUrl]);

  const handleSave = async () => {
    await setMutation.mutateAsync(fullUrl);
    setDirty(false);
  };

  const handleTest = async () => {
    if (fullUrl) {
      await testMutation.mutateAsync(fullUrl);
    }
  };

  const handleScan = async () => {
    const result = await scanMutation.mutateAsync();
    setDetected(result);
  };

  const handleSelect = (proxyUrl: string) => {
    const { baseUrl, username: u, password: p } = extractAuth(proxyUrl);
    setUrl(baseUrl);
    setUsername(u);
    setPassword(p);
    setDirty(true);
    setDetected([]);
  };

  const handleClear = () => {
    setUrl("");
    setUsername("");
    setPassword("");
    setDirty(true);
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && dirty && !setMutation.isPending) {
      handleSave();
    }
  };

  // Show loading only during the initial load when no data is available.
  if (isLoading && savedUrl === undefined) {
    return (
      <div className="flex items-center justify-center p-4">
        <Loader2 className="h-5 w-5 animate-spin text-muted-foreground" />
      </div>
    );
  }

  return (
    <div className="space-y-3">
      {/* Description */}
      <p className="text-sm text-muted-foreground">
        {t("settings.globalProxy.hint")}
      </p>

      {/* Proxy address input and actions */}
      <div className="flex gap-2">
        <Input
          placeholder="http://127.0.0.1:7890 / socks5://127.0.0.1:1080"
          value={url}
          onChange={(e) => {
            setUrl(e.target.value);
            setDirty(true);
          }}
          onKeyDown={handleKeyDown}
          className="font-mono text-sm flex-1"
        />
        <Button
          variant="outline"
          size="icon"
          disabled={scanMutation.isPending}
          onClick={handleScan}
          title={t("settings.globalProxy.scan")}
        >
          {scanMutation.isPending ? (
            <Loader2 className="h-4 w-4 animate-spin" />
          ) : (
            <Search className="h-4 w-4" />
          )}
        </Button>
        <Button
          variant="outline"
          size="icon"
          disabled={!fullUrl || testMutation.isPending}
          onClick={handleTest}
          title={t("settings.globalProxy.test")}
        >
          {testMutation.isPending ? (
            <Loader2 className="h-4 w-4 animate-spin" />
          ) : (
            <TestTube2 className="h-4 w-4" />
          )}
        </Button>
        <Button
          variant="outline"
          size="icon"
          disabled={!url && !username && !password}
          onClick={handleClear}
          title={t("settings.globalProxy.clear")}
        >
          <X className="h-4 w-4" />
        </Button>
        <Button
          onClick={handleSave}
          disabled={!dirty || setMutation.isPending}
          size="sm"
        >
          {setMutation.isPending && (
            <Loader2 className="mr-2 h-4 w-4 animate-spin" />
          )}
          {t("common.save")}
        </Button>
      </div>

      {/* Optional credentials: username and password */}
      <div className="flex gap-2">
        <Input
          placeholder={t("settings.globalProxy.username")}
          value={username}
          onChange={(e) => {
            setUsername(e.target.value);
            setDirty(true);
          }}
          onKeyDown={handleKeyDown}
          className="font-mono text-sm flex-1"
        />
        <div className="relative flex-1">
          <Input
            type={showPassword ? "text" : "password"}
            placeholder={t("settings.globalProxy.password")}
            value={password}
            onChange={(e) => {
              setPassword(e.target.value);
              setDirty(true);
            }}
            onKeyDown={handleKeyDown}
            className="font-mono text-sm pr-10"
          />
          <Button
            type="button"
            variant="ghost"
            size="icon"
            className="absolute right-0 top-0 h-full px-3 hover:bg-transparent"
            onClick={() => setShowPassword(!showPassword)}
            tabIndex={-1}
          >
            {showPassword ? (
              <EyeOff className="h-4 w-4 text-muted-foreground" />
            ) : (
              <Eye className="h-4 w-4 text-muted-foreground" />
            )}
          </Button>
        </div>
      </div>

      {/* Scan results */}
      {detected.length > 0 && (
        <div className="flex flex-wrap gap-2">
          {detected.map((p) => (
            <Button
              key={p.url}
              variant="secondary"
              size="sm"
              onClick={() => handleSelect(p.url)}
              className="font-mono text-xs"
            >
              {p.url}
            </Button>
          ))}
        </div>
      )}
    </div>
  );
}

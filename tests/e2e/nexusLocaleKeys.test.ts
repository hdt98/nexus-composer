import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const LOCALES_DIR = "src/i18n/locales";

type Catalog = Record<string, unknown>;

function loadCatalog(locale: string): Catalog {
  return JSON.parse(readFileSync(join(LOCALES_DIR, `${locale}.json`), "utf-8"));
}

function lookup(catalog: Catalog, dottedKey: string): unknown {
  return dottedKey.split(".").reduce<unknown>((value, segment) => {
    if (value && typeof value === "object" && segment in value) {
      return (value as Record<string, unknown>)[segment];
    }
    return undefined;
  }, catalog);
}

function leafKeys(value: unknown, prefix = ""): string[] {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    return Object.entries(value).flatMap(([key, child]) =>
      leafKeys(child, prefix ? `${prefix}.${key}` : key),
    );
  }
  return [prefix];
}

function placeholders(value: unknown): string[] {
  if (typeof value !== "string") return [];
  return (value.match(/\{\{\w+\}\}/g) ?? []).sort();
}

const BATCHED_KEYS = [
  "provider.duplicateLiveIdsLoadFailed",
  "universalProvider.addedButSyncFailed",
  "providerForm.providerKeyStatusLoading",
  "codexConfig.noCommonConfigToApply",
  "failover.tooltip.takeoverRequired",
  "notifications.proxyReasonClaudeDesktop",
  "proxy.server.stopped",
  "proxy.server.stopFailed",
  "usage.unpriced",
];

const TARGET_LOCAL_VI_P0 = {
  "usage.title": "Thống kê sử dụng",
  "common.about": "Giới thiệu",
  "settings.tabGeneral": "Tổng quan",
  "settings.tabProxy": "Nexus",
  "settings.tabAuth": "Tài khoản",
  "settings.tabAdvanced": "Nâng cao",
  "header.windowMinimize": "Thu nhỏ cửa sổ",
  "header.windowMaximize": "Phóng to cửa sổ",
  "header.windowRestore": "Khôi phục cửa sổ",
  "header.windowClose": "Đóng cửa sổ",
  "header.addProvider": "Thêm dịch vụ",
  "header.enterEditMode": "Vào chế độ sửa",
  "header.exitEditMode": "Thoát chế độ sửa",
  "apiKeyInput.placeholder": "Nhập khóa API",
  "apiKeyInput.show": "Hiện khóa API",
  "apiKeyInput.hide": "Ẩn khóa API",
  "jsonEditor.invalidJson": "Định dạng JSON không hợp lệ",
  "commonConfig.guideTitle": "Đoạn cấu hình dùng chung là gì?",
  "commonConfig.emptyTitle": "Chưa có đoạn cấu hình dùng chung",
  "claudeConfig.writeCommonConfig": "Áp dụng cấu hình dùng chung",
  "claudeConfig.editCommonConfig": "Sửa cấu hình dùng chung",
  "claudeConfig.extractFromCurrent": "Trích xuất từ trình soạn thảo",
  "geminiConfig.envFile": "Biến môi trường (.env)",
  "geminiConfig.writeCommonConfig": "Áp dụng cấu hình dùng chung",
  "geminiConfig.extractFailed": "Trích xuất thất bại: {{error}}",
  "deeplink.confirmImport": "Xác nhận nhập dịch vụ",
  "deeplink.importPrompt": "Nhập lời nhắc",
  "deeplink.importMcp": "Nhập máy chủ MCP",
  "deeplink.importSkill": "Thêm kho kỹ năng",
  "deeplink.providerName": "Tên dịch vụ",
  "deeplink.apiKey": "Khóa API",
  "settings.authCenter.title": "Tài khoản OAuth",
  "settings.authCenter.description":
    "Quản lý đăng nhập OAuth cho các tài khoản và gói đăng ký được hỗ trợ. Hãy kiểm tra điều khoản của từng dịch vụ trước khi dùng.",
  "settings.authCenter.copilotDescription": "Quản lý tài khoản GitHub Copilot",
  "settings.authCenter.codexOauthDescription": "Quản lý tài khoản ChatGPT",
  "copilot.authStatus": "Trạng thái xác thực",
  "copilot.notAuthenticated": "Chưa xác thực",
  "copilot.loginWithGitHub": "Đăng nhập bằng GitHub",
  "copilot.deploymentType": "Loại triển khai GitHub",
  "copilot.selectAccount": "Chọn tài khoản",
  "copilot.loggedInAccounts": "Tài khoản đã đăng nhập",
  "copilot.logoutAll": "Đăng xuất mọi tài khoản",
  "codexOauth.authStatus": "Trạng thái xác thực",
  "codexOauth.notAuthenticated": "Chưa xác thực",
  "codexOauth.loginWithChatGPT": "Đăng nhập bằng ChatGPT",
  "codexOauth.selectAccount": "Chọn tài khoản",
  "codexOauth.loggedInAccounts": "Tài khoản đã đăng nhập",
  "codexOauth.logoutAll": "Đăng xuất mọi tài khoản",
  "settings.advanced.configDir.title": "Thư mục cấu hình",
  "settings.advanced.configDir.description":
    "Quản lý đường dẫn lưu cấu hình của Claude, Codex, Gemini và các công cụ khác",
  "settings.advanced.proxy.title": "Nexus",
  "settings.advanced.proxy.description":
    "Bật/tắt Nexus, xem trạng thái và thông tin cổng",
  "settings.advanced.proxy.enableFeature": "Hiển thị nút Nexus trên trang chủ",
  "settings.advanced.proxy.enableFailoverToggle":
    "Hiển thị nút chuyển đổi dự phòng trên trang chủ",
  "settings.advanced.proxy.running": "Đang chạy",
  "settings.advanced.proxy.stopped": "Đã dừng",
  "settings.advanced.modelTest.title": "Cấu hình kiểm thử mô hình",
  "settings.advanced.failover.title": "Chuyển đổi dự phòng tự động",
  "settings.advanced.data.title": "Quản lý dữ liệu",
  "settings.advanced.backup.title": "Sao lưu và khôi phục",
  "settings.advanced.cloudSync.title": "Đồng bộ đám mây",
  "settings.advanced.rectifier.title": "Tự sửa lỗi API",
  "settings.advanced.logConfig.title": "Quản lý log",
  "confirm.proxy.title": "Bật Nexus",
  "confirm.failover.title": "Bật chuyển đổi dự phòng",
  "proxy.panel.serviceAddress": "Địa chỉ Nexus",
  "proxy.panel.stoppedTitle": "Nexus đang tắt",
  "proxy.panel.openSettings": "Cấu hình Nexus",
  "proxy.settings.title": "Cài đặt Nexus",
  "proxy.settings.basic.title": "Cài đặt cơ bản",
  "proxy.settings.advanced.title": "Tham số nâng cao",
  "proxy.settings.fields.listenAddress.label": "Địa chỉ nhận kết nối",
  "proxy.settings.fields.listenPort.label": "Cổng nhận kết nối",
  "proxy.settings.fields.enableLogging.label": "Bật ghi log",
  "proxyConfig.proxyEnabled": "Bật Nexus",
  "proxyConfig.appTakeover": "Ứng dụng dùng Nexus",
  "proxy.failover.proxyRequired":
    "Cần khởi động Nexus trước khi cấu hình chuyển đổi dự phòng",
  "proxy.failover.autoSwitch": "Chuyển đổi dự phòng tự động",
  "proxy.failoverQueue.title": "Hàng đợi dự phòng",
  "proxy.failoverQueue.description":
    "Quản lý thứ tự dự phòng cho từng ứng dụng",
  "proxy.autoFailover.retrySettings": "Cài đặt thử lại và thời gian chờ",
  "proxy.autoFailover.circuitBreakerSettings": "Cài đặt bộ ngắt mạch",
  "streamCheck.testModels": "Mô hình kiểm thử",
  "streamCheck.checkParams": "Tham số kiểm tra",
  "settings.appVisibility.title": "Hiển thị trên trang chủ",
  "settings.appVisibility.description":
    "Chọn các ứng dụng hiển thị trên trang chủ",
  "settings.skillStorage.title": "Vị trí lưu trữ kỹ năng",
  "settings.skillStorage.description":
    "Chọn nơi Nexus Composer lưu bản chính của các kỹ năng",
  "settings.skillStorage.ccSwitchHint":
    "Skill được lưu trong thư mục dữ liệu Nexus Composer và đồng bộ với từng ứng dụng bằng liên kết mềm hoặc bằng cách sao chép.",
  "settings.skillSync.title": "Phương thức đồng bộ kỹ năng",
  "settings.skillSync.description": "Chọn cách đồng bộ các tệp kỹ năng",
  "settings.skillSync.symlink": "Liên kết mềm",
  "settings.skillSync.copy": "Sao chép tệp",
  "settings.skillSync.symlinkHint":
    "Liên kết mềm giúp tiết kiệm dung lượng đĩa và đồng bộ theo thời gian thực. Lưu ý: Có thể cần quyền quản trị viên hoặc Chế độ nhà phát triển trên Windows.",
  "settings.codexAuth": "Tính năng nâng cao của ứng dụng Codex",
  "settings.preserveCodexOfficialAuthOnSwitch":
    "Giữ đăng nhập chính thức khi chuyển sang dịch vụ bên thứ ba",
  "settings.preserveCodexOfficialAuthOnSwitchDescription":
    "Khi bật, bạn vẫn có thể dùng các plugin chính thức, tính năng điều khiển từ xa trên thiết bị di động và các tính năng khác của ứng dụng Codex trong khi sử dụng API bên thứ ba.",
  "settings.unifyCodexSessionHistory": "Hợp nhất lịch sử phiên Codex",
  "settings.unifyCodexSessionHistoryDescription":
    'Khi bật, gói đăng ký chính thức dùng chung mã dịch vụ "custom" để phiên chính thức và bên thứ ba cùng xuất hiện trong một danh sách lịch sử. Bạn cũng có thể di chuyển phiên chính thức hiện có vào danh sách này sau khi tạo bản sao lưu. Khi tắt, bạn có thể khôi phục các phiên đã di chuyển từ bản sao lưu. Lưu ý: tiếp tục phiên cũ bằng dịch vụ khác có thể thất bại vì encrypted_content chỉ giải mã được bởi dịch vụ đã tạo phiên.',
  "settings.enableClaudePluginIntegration":
    "Áp dụng cho tiện ích mở rộng Claude Code",
  "settings.enableClaudePluginIntegrationDescription":
    "Khi bật, dịch vụ của tiện ích mở rộng Claude Code trong VS Code sẽ chuyển theo ứng dụng này.",
  "settings.skipClaudeOnboarding":
    "Bỏ qua bước xác nhận trong lần chạy đầu của Claude Code",
  "settings.skipClaudeOnboardingDescription":
    "Khi bật, Claude Code sẽ bỏ qua bước xác nhận trong lần chạy đầu.",
  "settings.minimizeToTrayDescription":
    "Khi chọn, việc nhấp nút đóng sẽ ẩn ứng dụng vào khay hệ thống; nếu không, ứng dụng sẽ thoát hoàn toàn.",
  "settings.terminal.title": "Terminal ưu tiên",
  "settings.terminal.description":
    "Chọn ứng dụng terminal sẽ dùng khi nhấp vào nút terminal",
  "settings.terminal.fallbackHint":
    "Nếu terminal đã chọn không khả dụng, hệ thống sẽ dùng terminal mặc định",
  "settings.importExport": "Nhập/xuất SQL",
  "settings.importExportHint":
    "Nhập hoặc xuất bản sao lưu SQL của cơ sở dữ liệu để di chuyển hoặc khôi phục. Chức năng nhập chỉ hỗ trợ bản sao lưu do Nexus Composer xuất ra.",
  "settings.backupManager.title": "Sao lưu cơ sở dữ liệu",
  "settings.backupManager.createBackup": "Sao lưu ngay",
  "settings.webdavSync.title": "Đồng bộ đám mây WebDAV",
  "settings.webdavSync.autoSync": "Tự động đồng bộ",
  "settings.s3Sync.enabled": "Bật đồng bộ S3",
  "settings.configDirectoryOverride": "Ghi đè thư mục cấu hình (nâng cao)",
  "settings.globalProxy.label": "Proxy mạng toàn cục",
  "providerPreset.label": "Preset dịch vụ",
  "providerPreset.custom": "Cấu hình tùy chỉnh",
  "usage.trends": "Xu hướng sử dụng",
  "usage.requestLogs": "Nhật ký lượt gọi",
  "usage.modelPricing": "Bảng giá mô hình",
  "usageScript.title": "Cấu hình thống kê sử dụng",
  "usageScript.enableUsageQuery": "Bật thống kê sử dụng",
  "provider.connectivityCheck": "Kiểm tra kết nối",
  "provider.noProviders": "Chưa có dịch vụ nào",
  "provider.importCurrent": "Nhập cấu hình hiện tại",
  "provider.currentlyUsing": "Đang sử dụng",
  "provider.blockedByProxyHint":
    "Không thể chuyển sang dịch vụ chính thức khi ứng dụng đang dùng Nexus",
  "provider.editProvider": "Sửa dịch vụ",
  "provider.deleteProvider": "Xóa dịch vụ",
  "provider.addClaudeProvider": "Thêm dịch vụ Claude Code",
  "provider.addCodexProvider": "Thêm dịch vụ Codex",
  "provider.addToConfig": "Thêm",
  "provider.removeFromConfig": "Gỡ",
  "provider.setAsDefault": "Đặt làm mặc định",
  "provider.configError": "Lỗi cấu hình",
  "provider.dragToReorder": "Kéo để sắp xếp lại",
  "provider.dragHandle": "Kéo để sắp xếp lại",
  "provider.searchPlaceholder": "Tìm theo tên, ghi chú hoặc URL...",
  "provider.searchAriaLabel": "Tìm dịch vụ",
  "provider.noSearchResults": "Không có dịch vụ nào khớp với tìm kiếm.",
  "provider.duplicate": "Nhân bản",
  "provider.configureUsage": "Cấu hình thống kê",
  "provider.openTerminal": "Mở terminal",
  "provider.terminalOpened": "Đã mở terminal",
  "provider.terminalOpenFailed": "Không thể mở terminal",
  "provider.name": "Tên dịch vụ",
  "provider.notes": "Ghi chú",
  "provider.configJson": "JSON cấu hình",
  "provider.writeCommonConfig": "Áp dụng cấu hình dùng chung",
  "provider.addProvider": "Thêm dịch vụ",
  "mcp.title": "Quản lý MCP",
  "mcp.claudeTitle": "Quản lý MCP cho Claude Code",
  "mcp.codexTitle": "Quản lý MCP cho Codex",
  "mcp.geminiTitle": "Quản lý MCP cho Gemini",
  "prompts.manage": "Lời nhắc",
  "prompts.title": "Quản lý lời nhắc {{appName}}",
  "skills.manage": "Kỹ năng",
  "skills.title": "Quản lý kỹ năng",
  "providerForm.manageAndTest": "Quản lý & kiểm thử",
  "providerForm.apiEndpoint": "Địa chỉ API",
  "providerForm.apiFormat": "Định dạng API",
  "codexConfig.writeCommonConfig": "Áp dụng cấu hình dùng chung",
  "codexConfig.modelName": "Tên mô hình",
  "codexConfig.upstreamFormatLabel": "Định dạng API của dịch vụ",
  "claudeCode.needsRouting": "Dùng Nexus",
  "claudeCode.noRoutingSupport": "Không hỗ trợ Nexus",
  "codex.needsRouting": "Dùng Nexus",
  "codex.noRoutingSupport": "Không hỗ trợ Nexus",
  "provider.enable": "Bật",
  "provider.inUse": "Đang dùng",
} as const;

const P0_PRODUCT_LABEL_KEYS = new Set([
  "settings.tabProxy",
  "settings.advanced.proxy.title",
]);

const FORBIDDEN_SCRIPT_REGEX =
  /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]/u;
const NON_LATIN_LETTER_REGEX = /(?!\p{Script=Latin})\p{Letter}/u;

const CONFIG_SOURCE_FILES = readdirSync("src/config")
  .filter((file) => file.endsWith(".ts"))
  .map((file) => join("src/config", file));

const CALL_SITE_FILES = [
  { path: "src/App.tsx", key: "provider.duplicateLiveIdsLoadFailed" },
  {
    path: "src/components/providers/AddProviderDialog.tsx",
    key: "universalProvider.addedButSyncFailed",
  },
  {
    path: "src/components/providers/forms/ProviderForm.tsx",
    key: "providerForm.providerKeyStatusLoading",
  },
  {
    path: "src/components/providers/forms/hooks/useCodexCommonConfig.ts",
    key: "codexConfig.noCommonConfigToApply",
  },
  {
    path: "src/components/proxy/FailoverToggle.tsx",
    key: "failover.tooltip.takeoverRequired",
  },
  {
    path: "src/hooks/useProviderActions.ts",
    key: "notifications.proxyReasonClaudeDesktop",
  },
  { path: "src/hooks/useProxyStatus.ts", key: "proxy.server.stopped" },
  { path: "src/hooks/useProxyStatus.ts", key: "proxy.server.stopFailed" },
  {
    path: "src/components/usage/RequestLogTable.tsx",
    key: "usage.unpriced",
  },
  {
    path: "src/components/usage/RequestDetailPanel.tsx",
    key: "usage.unpriced",
  },
];

function extractFallbacks(source: string, key: string): string[] {
  const quotedKey = `"${key}"`;
  const escapedKey = quotedKey.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const patterns = [
    new RegExp(escapedKey + "[^}]*?defaultValue:\\s*`([^`]*)`"),
    new RegExp(escapedKey + '[^}]*?defaultValue:\\s*"([^"]*)"'),
    new RegExp(escapedKey + '\\s*,\\s*"([^"]*)"\\s*\\)'),
  ];
  const fallbacks: string[] = [];
  let searchFrom = 0;

  while (true) {
    const keyIndex = source.indexOf(quotedKey, searchFrom);
    if (keyIndex === -1) break;
    const tail = source.slice(keyIndex, keyIndex + 500);
    const fallback = patterns
      .map((pattern) => tail.match(pattern)?.[1])
      .find((value): value is string => value !== undefined);
    if (fallback !== undefined) fallbacks.push(fallback);
    searchFrom = keyIndex + quotedKey.length;
  }

  return fallbacks;
}

describe("locale catalog parity for the focused batch", () => {
  const en = loadCatalog("en");
  const vi = loadCatalog("vi");

  it("keeps the complete English and Vietnamese leaf-key sets equal", () => {
    expect(leafKeys(en).sort()).toEqual(leafKeys(vi).sort());
  });

  it("keeps both catalogs free of forbidden scripts", () => {
    expect(FORBIDDEN_SCRIPT_REGEX.test(JSON.stringify(en))).toBe(false);
    expect(FORBIDDEN_SCRIPT_REGEX.test(JSON.stringify(vi))).toBe(false);
  });

  it("does not retain the unused Nexus home or preset namespace", () => {
    expect(en).not.toHaveProperty("nexus");
    expect(vi).not.toHaveProperty("nexus");
  });

  it("does not retain locale keys owned only by deleted UI surfaces", () => {
    const deletedSurfaceKeys = [
      "agents",
      "circuitBreaker",
      "common.toggleTheme",
      "usage.dataSources",
      "usage.dataSource",
      "usage.sessionSync",
    ] as const;

    for (const key of deletedSurfaceKeys) {
      expect(lookup(en, key), key).toBeUndefined();
      expect(lookup(vi, key), key).toBeUndefined();
    }

    for (const catalog of [en, vi]) {
      expect(lookup(catalog, "common.loadFailed")).toEqual(expect.any(String));
      expect(lookup(catalog, "common.retry")).toEqual(expect.any(String));
    }
  });

  it("describes every supported Claude Desktop role in role-sensitive copy", () => {
    const roleSensitiveKeys = [
      "modelMappingOffHint",
      "modelMappingOnHint",
      "routeMapHint",
      "directModelListHint",
      "directModelInvalid",
      "statusStaleRawModels",
    ] as const;

    for (const key of roleSensitiveKeys) {
      const enValue = lookup(en, `claudeDesktop.${key}`);
      const viValue = lookup(vi, `claudeDesktop.${key}`);
      for (const value of [enValue, viValue]) {
        expect(value, key).toEqual(expect.any(String));
        expect(value, key).toMatch(/Sonnet/i);
        expect(value, key).toMatch(/Opus/i);
        expect(value, key).toMatch(/Fable/i);
        expect(value, key).toMatch(/Haiku/i);
      }
      expect(viValue, key).not.toBe(enValue);
      expect(viValue, key).toMatch(/[ăâđêôơưà-ỹ]/i);
    }
  });

  it("keeps repaired Vietnamese provider and history copy translated and path-neutral", () => {
    const addProviderEn = lookup(en, "provider.addNewProvider");
    const addProviderVi = lookup(vi, "provider.addNewProvider");
    const historyEn = lookup(en, "confirm.unifyCodexHistory.message");
    const historyVi = lookup(vi, "confirm.unifyCodexHistory.message");

    expect(addProviderVi).toBe("Thêm dịch vụ mới");
    expect(addProviderVi).not.toBe(addProviderEn);
    expect(historyVi).not.toBe(historyEn);
    expect(historyVi).toMatch(/[ăâđêôơưà-ỹ]/i);
    expect(String(historyEn)).not.toContain("~/.nexus-composer");
    expect(String(historyVi)).not.toContain("~/.cc-switch");
  });

  it("fully localizes Session Manager copy while preserving interpolation", () => {
    const properNameKeys = new Set([
      "sessionManager.terminalTargetTerminal",
      "sessionManager.terminalTargetKitty",
    ]);
    const sessionKeys = leafKeys(
      lookup(en, "sessionManager"),
      "sessionManager",
    );

    for (const key of sessionKeys) {
      const enValue = lookup(en, key);
      const viValue = lookup(vi, key);
      expect(viValue, key).toEqual(expect.any(String));
      expect(placeholders(viValue), key).toEqual(placeholders(enValue));
      if (!properNameKeys.has(key)) {
        expect(viValue, key).not.toBe(enValue);
      }
    }

    expect(JSON.stringify(lookup(vi, "sessionManager"))).not.toMatch(
      /Session Manager|Search sessions|Provider filter|View mode|Unknown directory|Expand or collapse|Delete session|Loading sessions|No sessions found|Last active|Copy command|Conversation History/,
    );
  });

  it.each(Object.entries(TARGET_LOCAL_VI_P0))(
    "keeps the target-local Vietnamese P0 translated for %s",
    (key, expected) => {
      const enValue = lookup(en, key);
      const viValue = lookup(vi, key);

      expect(viValue).toBe(expected);
      expect(viValue).not.toBe(enValue);
      expect(placeholders(viValue)).toEqual(placeholders(enValue));
      if (
        key !== "providerForm.apiEndpoint" &&
        !P0_PRODUCT_LABEL_KEYS.has(key)
      ) {
        expect(viValue).toMatch(/[ăâđêôơưà-ỹ]/i);
      }
    },
  );

  it.each(BATCHED_KEYS)(
    "both catalogs define non-empty strings for %s",
    (key) => {
      for (const catalog of [en, vi]) {
        const value = lookup(catalog, key);
        expect(typeof value).toBe("string");
        expect((value as string).trim()).not.toBe("");
        expect(FORBIDDEN_SCRIPT_REGEX.test(value as string)).toBe(false);
      }
    },
  );

  it.each(BATCHED_KEYS)("both catalogs preserve placeholders for %s", (key) => {
    expect(placeholders(lookup(en, key))).toEqual(
      placeholders(lookup(vi, key)),
    );
  });
});

describe("focused call-site fallbacks", () => {
  it.each(CALL_SITE_FILES)(
    "$path uses script-safe fallbacks for $key",
    ({ path, key }) => {
      const fallbacks = extractFallbacks(readFileSync(path, "utf-8"), key);
      expect(fallbacks.length).toBeGreaterThan(0);
      for (const fallback of fallbacks) {
        expect(FORBIDDEN_SCRIPT_REGEX.test(fallback)).toBe(false);
      }
    },
  );
});

describe("configuration source language", () => {
  it.each(CONFIG_SOURCE_FILES)("%s contains only Latin-script text", (path) => {
    expect(NON_LATIN_LETTER_REGEX.test(readFileSync(path, "utf-8"))).toBe(
      false,
    );
  });
});

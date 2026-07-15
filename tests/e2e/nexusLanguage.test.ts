/**
 * Nexus Composer language system tests.
 *
 * Verifies that only English and Vietnamese locale files exist,
 * and the i18n config loads only these two languages.
 */
import { describe, expect, it } from "vitest";
import { readFileSync, existsSync } from "fs";

describe("Nexus Composer language system", () => {
  it("only en.json and vi.json locale files exist", () => {
    expect(existsSync("src/i18n/locales/en.json")).toBe(true);
    expect(existsSync("src/i18n/locales/vi.json")).toBe(true);
    expect(existsSync("src/i18n/locales/zh.json")).toBe(false);
    expect(existsSync("src/i18n/locales/zh-TW.json")).toBe(false);
    expect(existsSync("src/i18n/locales/ja.json")).toBe(false);
  });

  it("i18n config imports only en and vi", () => {
    const config = readFileSync("src/i18n/index.ts", "utf-8");
    expect(config).toContain('import en from "./locales/en.json"');
    expect(config).toContain('import vi from "./locales/vi.json"');
    expect(config).not.toContain('import ja from');
    expect(config).not.toContain('import zh from');
    expect(config).not.toContain('import zhTW from');
  });

  it("i18n config has only en and vi in resources", () => {
    const config = readFileSync("src/i18n/index.ts", "utf-8");
    expect(config).toMatch(/en:\s*\{[^}]*translation:\s*en/);
    expect(config).toMatch(/vi:\s*\{[^}]*translation:\s*vi/);
    expect(config).not.toMatch(/zh:/);
    expect(config).not.toMatch(/ja:/);
  });

  it("default language is English", () => {
    const config = readFileSync("src/i18n/index.ts", "utf-8");
    expect(config).toContain('DEFAULT_LANGUAGE: Language = "en"');
  });

  it("en.json has Vietnamese language option", () => {
    const en = JSON.parse(readFileSync("src/i18n/locales/en.json", "utf-8"));
    expect(en.settings.languageOptionVietnamese).toBeTruthy();
  });

  it("en.json does not have Chinese or Japanese options", () => {
    const en = JSON.parse(readFileSync("src/i18n/locales/en.json", "utf-8"));
    expect(en.settings.languageOptionChinese).toBeUndefined();
    expect(en.settings.languageOptionJapanese).toBeUndefined();
  });

  it("vi.json has Vietnamese language option with diacritics", () => {
    const vi = JSON.parse(readFileSync("src/i18n/locales/vi.json", "utf-8"));
    expect(vi.settings.languageOptionVietnamese).toBe("Tiếng Việt");
  });

  it("vi.json has English language option", () => {
    const vi = JSON.parse(readFileSync("src/i18n/locales/vi.json", "utf-8"));
    expect(vi.settings.languageOptionEnglish).toBe("English");
  });

  it("keeps Request Detail concise and fully Vietnamese", () => {
    const vi = JSON.parse(readFileSync("src/i18n/locales/vi.json", "utf-8"));

    expect(vi.usage).toMatchObject({
      requestDetail: "Chi tiết lượt gọi",
      requestNotFound: "Không tìm thấy lượt gọi",
      basicInfo: "Thông tin chung",
      requestId: "ID trong Nexus",
      correlationId: "ID phía máy chủ",
      copyCorrelationId: "Sao chép ID phía máy chủ",
      time: "Thời gian",
      provider: "Nhà cung cấp",
      unknownProvider: "Không rõ",
      appType: "Ứng dụng",
      model: "Mô hình",
      requestModel: "Mô hình yêu cầu",
      pricingModel: "Mô hình tính giá",
      status: "Trạng thái",
      tokenUsage: "Mức dùng token",
      inputTokens: "Đầu vào mới",
      rawInputLabel: "Tổng đầu vào",
      outputTokens: "Đầu ra",
      cacheReadTokens: "Đọc cache",
      cacheCreationTokens: "Ghi cache",
      totalTokens: "Tổng token mới",
      costBreakdown: "Chi phí",
      inputCost: "Chi phí đầu vào",
      outputCost: "Chi phí đầu ra",
      cacheReadCost: "Chi phí đọc cache",
      cacheCreationCost: "Chi phí ghi cache",
      costMultiplier: "Hệ số tính giá",
      baseCost: "Gốc",
      totalCost: "Tổng chi phí",
      withMultiplier: "đã áp dụng hệ số",
      unpriced: "Chưa có đơn giá",
      performance: "Hiệu năng",
      latency: "Độ trễ",
      errorMessage: "Lỗi",
    });
  });

  it("keeps Request Detail source free of untranslated Han fallbacks", () => {
    const source = readFileSync(
      "src/components/usage/RequestDetailPanel.tsx",
      "utf-8",
    );

    expect(source).not.toMatch(/\p{Script=Han}/u);
  });

  it("LanguageSettings component only has en and vi buttons", () => {
    const content = readFileSync(
      "src/components/settings/LanguageSettings.tsx",
      "utf-8",
    );
    expect(content).toContain('"en"');
    expect(content).toContain('"vi"');
    expect(content).not.toContain('"zh"');
    expect(content).not.toContain('"zh-TW"');
    expect(content).not.toContain('"ja"');
  });
});

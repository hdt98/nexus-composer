/**
 * Nexus Composer language system tests.
 *
 * Verifies the English and Vietnamese catalogs and i18n configuration.
 */
import { describe, expect, it } from "vitest";
import { existsSync, readFileSync, readdirSync } from "fs";
import { join } from "path";

type FlatCatalog = Map<string, string>;

const LOCALE_FILES = {
  en: "src/i18n/locales/en.json",
  vi: "src/i18n/locales/vi.json",
} as const;

function flattenCatalog(
  value: unknown,
  prefix = "",
  result: FlatCatalog = new Map(),
): FlatCatalog {
  if (typeof value === "string") {
    result.set(prefix, value);
    return result;
  }

  if (!value || Array.isArray(value) || typeof value !== "object") {
    throw new Error(`Unsupported locale value at ${prefix || "<root>"}`);
  }

  for (const [key, child] of Object.entries(value)) {
    flattenCatalog(child, prefix ? `${prefix}.${key}` : key, result);
  }
  return result;
}

function readCatalog(path: string): FlatCatalog {
  return flattenCatalog(JSON.parse(readFileSync(path, "utf-8")));
}

function sourceFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return path.endsWith(".ts") || path.endsWith(".tsx") ? [path] : [];
  });
}

function literalTranslationKeys(files: string[]): string[] {
  return files.flatMap((file) => {
    const source = readFileSync(file, "utf-8");
    return [...source.matchAll(/\bt\(\s*(["'`])([^"'`]+)\1/g)]
      .map((match) => match[2])
      .filter((key) => !key.includes("${"));
  });
}

function sourceTranslationKeys(
  files: string[],
  catalogRoots: Set<string>,
): string[] {
  return files.flatMap((file) => {
    const source = readFileSync(file, "utf-8");
    return [...source.matchAll(/(["'`])([A-Za-z][\w-]*(?:\.[\w-]+)+)\1/g)]
      .map((match) => match[2])
      .filter((key) => catalogRoots.has(key.split(".")[0]));
  });
}

function missingCatalogKeys(
  keys: string[],
  catalogs: Record<string, FlatCatalog>,
): string[] {
  return [...new Set(keys)]
    .sort()
    .flatMap((key) =>
      Object.entries(catalogs).flatMap(([language, catalog]) =>
        catalog.has(key) ? [] : [`${language}:${key}`],
      ),
    );
}

function interpolationTokens(value: string): string[] {
  return [...value.matchAll(/{{\s*([^},\s]+)[^}]*}}/g)]
    .map((match) => match[1])
    .sort();
}

const catalogs = {
  en: readCatalog(LOCALE_FILES.en),
  vi: readCatalog(LOCALE_FILES.vi),
};
const catalogRoots = new Set(
  [...catalogs.en.keys()].map((key) => key.split(".")[0]),
);
const appSourceFiles = sourceFiles("src");

describe("Nexus Composer language system", () => {
  it("keeps the English and Vietnamese locale trees in parity", () => {
    expect([...catalogs.vi.keys()].sort()).toEqual(
      [...catalogs.en.keys()].sort(),
    );
  });

  it("keeps interpolation tokens in parity", () => {
    const mismatches = [...catalogs.en].flatMap(([key, enValue]) => {
      const enTokens = interpolationTokens(enValue);
      const viTokens = interpolationTokens(catalogs.vi.get(key) ?? "");
      return JSON.stringify(enTokens) === JSON.stringify(viTokens)
        ? []
        : [{ key, enTokens, viTokens }];
    });

    expect(mismatches).toEqual([]);
  });

  it("has both locale entries for every literal translation call", () => {
    expect(
      missingCatalogKeys(literalTranslationKeys(appSourceFiles), catalogs),
    ).toEqual([]);
  });

  it("has both locale entries for mapped runtime translation keys", () => {
    const keys = sourceTranslationKeys(appSourceFiles, catalogRoots);

    expect(keys).toContain("subscription.thirtyDay");
    expect(missingCatalogKeys(keys, catalogs)).toEqual([]);
  });

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
    expect(config).not.toContain("import ja from");
    expect(config).not.toContain("import zh from");
    expect(config).not.toContain("import zhTW from");
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

  it("spells the Vietnamese language name correctly in both locales", () => {
    const en = JSON.parse(readFileSync("src/i18n/locales/en.json", "utf-8"));
    const vi = JSON.parse(readFileSync("src/i18n/locales/vi.json", "utf-8"));

    expect(en.settings.languageOptionVietnamese).toBe("Tiếng Việt");
    expect(vi.settings.languageOptionVietnamese).toBe("Tiếng Việt");
  });

  it("en.json does not have Chinese or Japanese options", () => {
    const en = JSON.parse(readFileSync("src/i18n/locales/en.json", "utf-8"));
    expect(en.settings.languageOptionChinese).toBeUndefined();
    expect(en.settings.languageOptionJapanese).toBeUndefined();
  });

  it("vi.json has English language option", () => {
    const vi = JSON.parse(readFileSync("src/i18n/locales/vi.json", "utf-8"));
    expect(vi.settings.languageOptionEnglish).toBe("English");
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

  it("keeps first-run copy implementation-neutral", () => {
    const en = JSON.parse(readFileSync("src/i18n/locales/en.json", "utf-8"));
    const vi = JSON.parse(readFileSync("src/i18n/locales/vi.json", "utf-8"));
    const implementationDetails =
      /sglang|ssh|kubernetes|127\.0\.0\.1|30000|30001/i;

    for (const notice of [en.firstRunNotice, vi.firstRunNotice]) {
      const copy = `${notice.bodyDefault} ${notice.bodyOfficial}`;
      expect(copy).toContain("Nexus");
      expect(copy).not.toMatch(implementationDetails);
    }
  });

  it("does not render Han-script preset labels", () => {
    const files = [
      "src/config/claudeDesktopProviderPresets.ts",
      "src/config/opencodeProviderPresets.ts",
      "src/config/openclawProviderPresets.ts",
      "src/config/hermesProviderPresets.ts",
      "src/config/geminiProviderPresets.ts",
      "src/config/universalProviderPresets.ts",
      "src/config/codingPlanProviders.ts",
      "src/icons/extracted/metadata.ts",
    ];

    for (const file of files) {
      const source = readFileSync(file, "utf-8");
      const renderedValues = [
        ...source.matchAll(
          /(?:name|description|label|displayName):\s*"([^"]*)"/g,
        ),
      ].map((match) => match[1]);

      expect(
        renderedValues.filter((value) => /\p{Script=Han}/u.test(value)),
      ).toEqual([]);
    }
  });
});

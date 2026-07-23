/**
 * Nexus Composer switch-back-to-official fix tests.
 *
 * Verifies that Codex can use its atomic switch-back path while
 * non-transactional clients remain fail-closed during takeover.
 */
import { describe, expect, it } from "vitest";
import { readFileSync } from "fs";

describe("Switch-back-to-official fix", () => {
  it("ProviderActions does not have isOfficialBlockedByProxy prop", () => {
    const content = readFileSync(
      "src/components/providers/ProviderActions.tsx",
      "utf-8",
    );
    expect(content).not.toContain("isOfficialBlockedByProxy");
  });

  it("ProviderCard does not compute isOfficialBlockedByProxy", () => {
    const content = readFileSync(
      "src/components/providers/ProviderCard.tsx",
      "utf-8",
    );
    expect(content).not.toContain("isOfficialBlockedByProxy");
  });

  it("App keeps durable takeover independent of listener liveness", () => {
    const content = readFileSync("src/App.tsx", "utf-8");

    expect(content).toMatch(
      /useProviderActions\(\s*activeApp,\s*isProxyRunning,\s*isCurrentAppTakeoverActive,?\s*\)/,
    );
    expect(content).toMatch(
      /<ProviderList[\s\S]*?isProxyTakeover=\{isCurrentAppTakeoverActive\}/,
    );
  });

  it("provider mod.rs blocks non-atomic Official switches", () => {
    const content = readFileSync(
      "src-tauri/src/services/provider/mod.rs",
      "utf-8",
    );
    expect(content).toContain("switch.official_blocked_by_proxy");
    expect(content).toContain("switch_codex_routing_atomic");
  });
});

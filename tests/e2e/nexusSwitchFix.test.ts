/**
 * Nexus Composer switch-back-to-official fix tests.
 *
 * Verifies that the ProviderActions component no longer blocks
 * switching to Official providers when proxy takeover is active.
 * The fix removed the isOfficialBlockedByProxy check.
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

  it("provider mod.rs auto-disables takeover when switching to official", () => {
    const content = readFileSync(
      "src-tauri/src/services/provider/mod.rs",
      "utf-8",
    );
    // The fix should auto-disable takeover instead of returning an error
    expect(content).not.toContain("switch.official_blocked_by_proxy");
    
    // Should call set_takeover_for_app(false) for official providers
    expect(content).toContain("set_takeover_for_app(app_type.as_str(), false)");
    expect(content).toContain("set_takeover_for_app(app_type.as_str(), false)");
  });
});

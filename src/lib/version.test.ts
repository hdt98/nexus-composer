import { describe, it, expect } from "vitest";
import { compareVersions, isUpdateAvailable } from "./version";

describe("compareVersions", () => {
  it("compares major, minor, and patch versions", () => {
    expect(compareVersions("2.1.156", "2.1.154")).toBeGreaterThan(0);
    expect(compareVersions("2.1.154", "2.1.156")).toBeLessThan(0);
    expect(compareVersions("2.2.0", "2.1.999")).toBeGreaterThan(0);
    expect(compareVersions("3.0.0", "2.9.9")).toBeGreaterThan(0);
    expect(compareVersions("2.1.156", "2.1.156")).toBe(0);
  });

  it("orders prereleases below a release with the same core version", () => {
    expect(compareVersions("2.1.156-beta.1", "2.1.156")).toBeLessThan(0);
    expect(compareVersions("2.1.156", "2.1.156-rc.1")).toBeGreaterThan(0);
  });

  it("compares prerelease identifiers according to semver ordering", () => {
    expect(compareVersions("1.0.0-beta.2", "1.0.0-beta.11")).toBeLessThan(0);
    expect(compareVersions("1.0.0-alpha", "1.0.0-beta")).toBeLessThan(0);
    expect(compareVersions("1.0.0-beta", "1.0.0-beta.1")).toBeLessThan(0);
  });

  it("conservatively returns zero for unparseable versions", () => {
    expect(compareVersions("", "2.1.154")).toBe(0);
    expect(compareVersions("unknown", "2.1.154")).toBe(0);
  });
});

describe("isUpdateAvailable", () => {
  it("reports an update only when latest is strictly newer", () => {
    expect(isUpdateAvailable("2.1.154", "2.1.156")).toBe(true);
  });

  it("does not report an update when a preview build is ahead of latest", () => {
    // Claude Code's next channel can be at 156 while npm latest remains at 154.
    expect(isUpdateAvailable("2.1.156", "2.1.154")).toBe(false);
  });

  it("does not report an update for equal versions", () => {
    expect(isUpdateAvailable("2.1.156", "2.1.156")).toBe(false);
  });

  it("does not report an update when either version is missing", () => {
    expect(isUpdateAvailable(undefined, "2.1.156")).toBe(false);
    expect(isUpdateAvailable("2.1.156", null)).toBe(false);
    expect(isUpdateAvailable("", "")).toBe(false);
  });
});

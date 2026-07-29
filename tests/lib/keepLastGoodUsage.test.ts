import { describe, it, expect } from "vitest";
import {
  resolveDisplayUsage,
  isTransientUsageError,
  KEEP_LAST_GOOD_MS,
  type LastGoodUsage,
} from "@/lib/query/queries";
import type { UsageResult } from "@/types";

// Pure keep-last-good decision logic: only transient or network failures continue
// showing the last success within KEEP_LAST_GOOD_MS. Deterministic failures such as
// authentication, empty keys, and unknown providers must surface immediately.

const ok = (remaining: number): UsageResult => ({
  success: true,
  data: [{ remaining, unit: "USD" }],
});
// Default to a transient network error; pass deterministic failures explicitly.
const fail = (error = "Network error: connection reset"): UsageResult => ({
  success: false,
  error,
});

const T0 = 1_000_000_000_000; // Arbitrary baseline time in milliseconds.

describe("isTransientUsageError", () => {
  it("treats network failures as transient", () => {
    expect(isTransientUsageError(fail("Network error: timed out"))).toBe(true);
    expect(isTransientUsageError(fail("Request failed: timed out"))).toBe(true);
    expect(
      isTransientUsageError(fail("Request failed: connection timed out")),
    ).toBe(true);
    expect(isTransientUsageError(fail("Failed to read response: eof"))).toBe(
      true,
    );
    expect(isTransientUsageError(fail("Failed to read response: eof"))).toBe(
      true,
    );
  });

  it("treats deterministic failures as non-transient so they surface immediately", () => {
    expect(
      isTransientUsageError(fail("Authentication failed (HTTP 401)")),
    ).toBe(false);
    expect(isTransientUsageError(fail("API key is empty"))).toBe(false);
    expect(isTransientUsageError(fail("Unknown balance provider"))).toBe(false);
    expect(isTransientUsageError(fail("Unknown coding plan provider"))).toBe(
      false,
    );
    expect(isTransientUsageError(fail("API error (HTTP 400): bad"))).toBe(
      false,
    );
    expect(isTransientUsageError(fail("Failed to parse response: x"))).toBe(
      false,
    );
  });

  it("treats HTTP 5xx as transient and 4xx as non-transient", () => {
    expect(isTransientUsageError(fail("API error (HTTP 500): oops"))).toBe(
      true,
    );
    expect(
      isTransientUsageError(fail("HTTP 503 Service Unavailable : x")),
    ).toBe(true);
    expect(
      isTransientUsageError(fail("API error (HTTP 502): bad gateway")),
    ).toBe(true);
    expect(
      isTransientUsageError(fail("API error (HTTP 429): rate limited")),
    ).toBe(false);
    expect(
      isTransientUsageError(fail("Authentication failed (HTTP 403)")),
    ).toBe(false);
  });

  it("returns false for success or a missing error message", () => {
    expect(isTransientUsageError(ok(1))).toBe(false);
    expect(isTransientUsageError({ success: false })).toBe(false);
  });
});

describe("resolveDisplayUsage (keep-last-good)", () => {
  it("displays a success unchanged and records it with its query time", () => {
    const success = ok(42);
    const r = resolveDisplayUsage(success, T0, null, T0);
    expect(r.data).toBe(success);
    expect(r.lastQueriedAt).toBe(T0);
    expect(r.lastGood).toEqual({ data: success, at: T0 });
  });

  it("keeps showing a recent success during a transient failure", () => {
    const prev: LastGoodUsage = { data: ok(42), at: T0 };
    const now = T0 + KEEP_LAST_GOOD_MS - 1; // Still just inside the window.
    const r = resolveDisplayUsage(fail(), now, prev, now);
    expect(r.data).toBe(prev.data); // Display the previous success.
    expect(r.lastQueriedAt).toBe(T0); // Preserve the successful query's age.
    expect(r.lastGood).toBe(prev); // A failure does not update lastGood.
  });

  it("displays a transient failure when the previous success has expired", () => {
    const prev: LastGoodUsage = { data: ok(42), at: T0 };
    const now = T0 + KEEP_LAST_GOOD_MS; // Boundary: the snapshot expires exactly at 10 minutes.
    const failure = fail();
    const r = resolveDisplayUsage(failure, now, prev, now);
    expect(r.data).toBe(failure);
    expect(r.lastQueriedAt).toBe(now);
    expect(r.lastGood).toBe(prev);
  });

  it("surfaces deterministic failures immediately and clears lastGood", () => {
    const prev: LastGoodUsage = { data: ok(42), at: T0 };
    const now = T0 + 1000; // Well inside the window.
    for (const failure of [
      fail("Authentication failed (HTTP 401)"),
      fail("API key is empty"),
      fail("Unknown coding plan provider"),
    ]) {
      const r = resolveDisplayUsage(failure, now, prev, now);
      expect(r.data).toBe(failure); // Do not hide a deterministic failure.
      expect(r.lastQueriedAt).toBe(now);
      expect(r.lastGood).toBeNull(); // Clear the untrusted snapshot so it cannot reappear later.
    }
  });

  it("does not revive an old success after a deterministic failure clears lastGood", () => {
    // Record lastGood after a success.
    const afterSuccess = resolveDisplayUsage(ok(42), T0, null, T0);
    expect(afterSuccess.lastGood).not.toBeNull();
    // Surface a 401 authentication failure and clear lastGood.
    const afterAuthFail = resolveDisplayUsage(
      fail("Authentication failed (HTTP 401)"),
      T0 + 1000,
      afterSuccess.lastGood,
      T0 + 1000,
    );
    expect(afterAuthFail.lastGood).toBeNull();
    // A later transient network failure cannot revive the cleared success.
    const netFail = fail();
    const afterBlip = resolveDisplayUsage(
      netFail,
      T0 + 2000,
      afterAuthFail.lastGood,
      T0 + 2000,
    );
    expect(afterBlip.data).toBe(netFail);
    expect(afterBlip.lastGood).toBeNull();
  });

  it("displays a transient failure when no success has been recorded", () => {
    const failure = fail();
    const now = T0 + 5000;
    const r = resolveDisplayUsage(failure, now, null, now);
    expect(r.data).toBe(failure);
    expect(r.lastQueriedAt).toBe(now);
    expect(r.lastGood).toBeNull();
  });

  it("replaces an old lastGood snapshot with a new success", () => {
    const prev: LastGoodUsage = { data: ok(42), at: T0 };
    const fresh = ok(7);
    const now = T0 + 60_000;
    const r = resolveDisplayUsage(fresh, now, prev, now);
    expect(r.data).toBe(fresh);
    expect(r.lastGood).toEqual({ data: fresh, at: now });
  });

  it("keeps lastGood unchanged while raw data is loading", () => {
    const prev: LastGoodUsage = { data: ok(42), at: T0 };
    const r = resolveDisplayUsage(undefined, 0, prev, T0 + 1000);
    expect(r.data).toBeUndefined();
    expect(r.lastQueriedAt).toBeNull();
    expect(r.lastGood).toBe(prev);
  });

  it("uses the injected time for a success when dataUpdatedAt is zero", () => {
    const success = ok(1);
    const now = T0 + 123;
    const r = resolveDisplayUsage(success, 0, null, now);
    expect(r.lastGood).toEqual({ data: success, at: now });
  });
});

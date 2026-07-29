/**
 * Lightweight semver comparison for determining whether a tool update is available.
 *
 * A string `!==` comparison is insufficient because unequal versions do not imply
 * that the installed version is older. A prerelease channel such as Claude Code's
 * npm `next` tag may be newer than `latest`; treating it as outdated would create a
 * persistent false update prompt followed by a misleading unchanged-version warning.
 */

interface ParsedVersion {
  /** Core version tuple: [major, minor, patch]. */
  core: [number, number, number];
  /** Prerelease identifiers, such as ["beta", "1"] for "2.1.156-beta.1"; empty for a release. */
  pre: string[];
}

/** Parse "2.1.156" or "2.1.156-beta.1"; return null when parsing fails. */
function parseVersion(v: string): ParsedVersion | null {
  const m = v.trim().match(/^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?/);
  if (!m) return null;
  return {
    core: [Number(m[1]), Number(m[2]), Number(m[3])],
    pre: m[4] ? m[4].split(".") : [],
  };
}

/**
 * Compare prerelease identifiers according to semver:
 * - Two releases without prerelease identifiers are equal.
 * - A prerelease is lower than a release.
 * - For two prereleases, compare numeric identifiers numerically, numeric identifiers
 *   below non-numeric identifiers, and non-numeric identifiers by ASCII order.
 *   When prefixes match, the version with more identifiers is greater.
 */
function comparePre(a: string[], b: string[]): number {
  if (a.length === 0 && b.length === 0) return 0;
  if (a.length === 0) return 1;
  if (b.length === 0) return -1;
  const len = Math.min(a.length, b.length);
  for (let i = 0; i < len; i++) {
    const ai = a[i];
    const bi = b[i];
    const aNum = /^\d+$/.test(ai);
    const bNum = /^\d+$/.test(bi);
    if (aNum && bNum) {
      const d = Number(ai) - Number(bi);
      if (d !== 0) return d < 0 ? -1 : 1;
    } else if (aNum) {
      return -1;
    } else if (bNum) {
      return 1;
    } else if (ai !== bi) {
      return ai < bi ? -1 : 1;
    }
  }
  if (a.length === b.length) return 0;
  return a.length < b.length ? -1 : 1;
}

/**
 * Compare two versions.
 * @returns A value above zero when a is newer than b, below zero when a is older,
 *   or zero when they are equal or cannot be compared. Invalid input returns zero
 *   conservatively so it does not trigger an update prompt.
 */
export function compareVersions(a: string, b: string): number {
  const pa = parseVersion(a);
  const pb = parseVersion(b);
  if (!pa || !pb) return 0;
  for (let i = 0; i < 3; i++) {
    const d = pa.core[i] - pb.core[i];
    if (d !== 0) return d < 0 ? -1 : 1;
  }
  return comparePre(pa.pre, pb.pre);
}

/**
 * Return true only when `latest` is strictly newer than `current`.
 * Return false whenever the installed version is at least as new as latest,
 * including prerelease channels that have advanced beyond latest.
 */
export function isUpdateAvailable(
  current: string | null | undefined,
  latest: string | null | undefined,
): boolean {
  if (!current || !latest) return false;
  return compareVersions(latest, current) > 0;
}

/**
 * Semantic version parsing and comparison.
 *
 * Moved here from `apps/desktop/src/chrome-app-update.ts` when the Electron shell
 * was deleted. The update index plans against it, and it has no Electron import —
 * it is a parser and a comparator.
 *
 * An unparseable input falls back to a locale compare with numeric collation
 * rather than throwing: the caller is deciding an ordering, and "1.10.0" must sort
 * after "1.9.0" even when one side is not a version at all.
 */

export interface SemverVersion {
  readonly major: number;
  readonly minor: number;
  readonly patch: number;
  readonly prerelease?: string;
}

export function parseSemver(raw: string): SemverVersion | undefined {
  const clean = raw.trim().replace(/^v/iu, "");
  const match = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$/u.exec(clean);
  if (!match) return undefined;
  return {
    major: Number.parseInt(match[1]!, 10),
    minor: Number.parseInt(match[2]!, 10),
    patch: Number.parseInt(match[3]!, 10),
    ...(match[4] !== undefined ? { prerelease: match[4] } : {}),
  };
}

/**
 * 比较两个语义化版本号。
 * 返回值：> 0 表示 a > b，< 0 表示 a < b，0 表示相等。
 */
export function compareSemver(aStr: string, bStr: string): number {
  const a = parseSemver(aStr);
  const b = parseSemver(bStr);
  if (!a || !b) {
    return aStr.localeCompare(bStr, undefined, { numeric: true, sensitivity: "base" });
  }
  if (a.major !== b.major) return a.major - b.major;
  if (a.minor !== b.minor) return a.minor - b.minor;
  if (a.patch !== b.patch) return a.patch - b.patch;
  if (a.prerelease === undefined && b.prerelease !== undefined) return 1;
  if (a.prerelease !== undefined && b.prerelease === undefined) return -1;
  if (a.prerelease !== undefined && b.prerelease !== undefined) {
    return a.prerelease.localeCompare(b.prerelease, undefined, { numeric: true });
  }
  return 0;
}

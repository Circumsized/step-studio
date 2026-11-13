/**
 * Runtime version ordering.
 *
 * Moved here from `apps/desktop/src/runtime-install.ts` when the Electron shell
 * was deleted. It is pure semver comparison with no Electron import, and
 * `scripts/build-update-assets-v2.mjs` imports it to decide whether a published
 * runtime is older than the one being built — which is the whole point of the
 * update index.
 *
 * `undefined` rather than a thrown error for an unparseable version: the caller's
 * question is "is this newer?", and a version it cannot read is not an answer to
 * that. The old Electron code returned `undefined` for the same reason and the
 * callers already handle it.
 */

interface ParsedRuntimeVersion {
  readonly core: readonly number[];
  readonly prerelease: readonly (number | string)[];
}

const RUNTIME_VERSION = /^(?:v)?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/u;

function parseRuntimeVersion(value: string): ParsedRuntimeVersion | undefined {
  const match = RUNTIME_VERSION.exec(value);
  if (match === null) return undefined;
  const core = [Number(match[1]), Number(match[2]), Number(match[3])] as const;
  if (core.some((part) => !Number.isSafeInteger(part))) return undefined;
  const prerelease: Array<number | string> = [];
  for (const identifier of match[4]?.split(".") ?? []) {
    if (/^(0|[1-9]\d*)$/u.test(identifier)) {
      const numeric = Number(identifier);
      if (!Number.isSafeInteger(numeric)) return undefined;
      prerelease.push(numeric);
    } else {
      prerelease.push(identifier);
    }
  }
  return { core, prerelease };
}

/**
 * Negative when `left` is older, positive when newer, `0` when equal, and
 * `undefined` when either side is not a version this can read.
 *
 * A release outranks its own prereleases, per semver — so `1.2.3` is newer than
 * `1.2.3-rc.1`. Numeric prerelease identifiers compare numerically and
 * alphanumeric ones lexically, again per semver.
 */
export function compareRuntimeVersions(left: string, right: string): number | undefined {
  const leftParsed = parseRuntimeVersion(left);
  const rightParsed = parseRuntimeVersion(right);
  if (leftParsed === undefined || rightParsed === undefined) return undefined;
  for (let index = 0; index < leftParsed.core.length; index += 1) {
    const leftPart = leftParsed.core[index] ?? 0;
    const rightPart = rightParsed.core[index] ?? 0;
    if (leftPart !== rightPart) return leftPart - rightPart;
  }
  if (leftParsed.prerelease.length === 0 || rightParsed.prerelease.length === 0) {
    if (leftParsed.prerelease.length === rightParsed.prerelease.length) return 0;
    return leftParsed.prerelease.length === 0 ? 1 : -1;
  }
  for (let index = 0; index < Math.max(leftParsed.prerelease.length, rightParsed.prerelease.length); index += 1) {
    const leftPart = leftParsed.prerelease[index];
    const rightPart = rightParsed.prerelease[index];
    if (leftPart === undefined) return -1;
    if (rightPart === undefined) return 1;
    if (typeof leftPart === "number" && typeof rightPart === "number") {
      if (leftPart !== rightPart) return leftPart - rightPart;
      continue;
    }
    // Numeric identifiers always rank below alphanumeric ones.
    if (typeof leftPart !== typeof rightPart) return typeof leftPart === "number" ? -1 : 1;
    if (leftPart !== rightPart) return String(leftPart).localeCompare(String(rightPart));
  }
  return 0;
}

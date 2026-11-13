/**
 * Version ordering, from `apps/desktop/src/runtime-install.ts`.
 *
 * The rules are semver's, and the ones worth pinning are the two that surprise:
 * a release outranks its own prereleases, and a numeric prerelease identifier
 * outranks an alphanumeric one. An unparseable version is `undefined`, not a
 * throw — the caller asks "is this newer?" and a version it cannot read is not an
 * answer.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { compareRuntimeVersions } from "../src/version-compare.js";

const older = (left: string, right: string) => {
  const order = compareRuntimeVersions(left, right);
  assert.ok(order !== undefined && order < 0, `${left} should be older than ${right}, got ${order}`);
};
const newer = (left: string, right: string) => older(right, left);
const same = (left: string, right: string) => {
  assert.equal(compareRuntimeVersions(left, right), 0, `${left} should equal ${right}`);
};

test("core parts decide first", () => {
  older("1.2.3", "1.2.4");
  older("1.2.3", "1.3.0");
  older("1.2.3", "2.0.0");
  older("9.9.9", "10.0.0");
  same("1.2.3", "1.2.3");
});

test("a leading v is accepted", () => {
  same("v1.2.3", "1.2.3");
  older("v1.2.3", "1.2.4");
});

test("a release outranks its own prereleases", () => {
  // The semver rule that decides whether an update index points at a release or
  // a candidate: `1.2.3` is newer than `1.2.3-rc.1`, not a tie.
  older("1.2.3-rc.1", "1.2.3");
  older("1.2.3-alpha", "1.2.3-beta");
  older("1.2.3-beta", "1.2.3");
});

test("numeric prerelease identifiers outrank alphanumeric ones", () => {
  // Also semver, and also load-bearing: `1.2.3-1` is newer than `1.2.3-alpha`.
  older("1.2.3-alpha", "1.2.3-1");
  older("1.2.3-1", "1.2.3-2");
});

test("a longer prerelease set outranks a shorter one when the shared prefix ties", () => {
  older("1.2.3-rc.1", "1.2.3-rc.1.1");
});

test("an unparseable version is undefined rather than a throw", () => {
  for (const bad of ["", "1.2", "1.2.3.4", "abc", "1.2.x", "01.2.3", "1.2.3-", "-1.2.3"]) {
    assert.equal(compareRuntimeVersions(bad, "1.2.3"), undefined, `${bad} is not a version`);
    assert.equal(compareRuntimeVersions("1.2.3", bad), undefined, `${bad} is not a version`);
  }
});

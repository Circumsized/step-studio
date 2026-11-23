// No mutable `uses:` references in any workflow.
//
// A tag is a promise the upstream can break: `actions/checkout@v4` today is not
// `actions/checkout@v4` after the maintainer repoints it, and the release job
// holds `contents: write`. Every ref here is pinned to a commit SHA with the
// human-readable version in a trailing comment, so a reviewer sees what it is and
// Dependabot can propose the bump.
//
// The one exemption is `dtolnay/rust-toolchain@stable`, which is a *branch*, not a
// tag. Pinning a branch to a SHA and calling it pinned is the worse lie: it looks
// immutable and silently stops tracking. It is left as a branch deliberately, and
// listed here so the exemption is a decision rather than an oversight.
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const EXEMPT_BRANCHES = new Set(["dtolnay/rust-toolchain@stable"]);

function ymlFiles(dir) {
  const out = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...ymlFiles(full));
    else if (entry.name.endsWith(".yml") || entry.name.endsWith(".yaml")) out.push(full);
  }
  return out;
}

const files = ymlFiles(join(root, ".github"));

test("every uses: is pinned to a commit SHA, or is a declared branch", () => {
  const mutable = [];
  for (const file of files) {
    const text = readFileSync(file, "utf8");
    for (const match of text.matchAll(/uses:\s*([^\s#]+)/g)) {
      const ref = match[1];
      if (ref.startsWith("./") || ref.startsWith("docker://")) continue;
      const at = ref.lastIndexOf("@");
      if (at <= 0) continue;
      const version = ref.slice(at + 1);
      // A full commit SHA is 40 hex characters.
      if (/^[0-9a-f]{40}$/.test(version)) continue;
      if (EXEMPT_BRANCHES.has(ref)) continue;
      mutable.push(`${file.replace(`${root}/`, "")}: ${ref}`);
    }
  }
  assert.deepEqual(mutable, [], `mutable action references: ${mutable.join(", ")}`);
});

test("a pinned ref carries its version in a comment", () => {
  // A bare SHA tells a reviewer nothing and leaves Dependabot unable to propose a
  // bump, which is how pins freeze.
  const uncommented = [];
  for (const file of files) {
    for (const line of readFileSync(file, "utf8").split("\n")) {
      const match = line.match(/uses:\s*([^\s#]+)@([0-9a-f]{40})\s*(#.*)?$/);
      if (match && !match[3]) uncommented.push(`${file.replace(`${root}/`, "")}: ${match[1]}`);
    }
  }
  assert.deepEqual(uncommented, [], `pinned without a version comment: ${uncommented.join(", ")}`);
});

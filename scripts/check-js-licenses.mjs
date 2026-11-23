// JavaScript license allow-list, aligned with deny.toml.
//
// cargo-deny gates the Rust side; until this existed nothing gated the npm side,
// so a copyleft dependency could arrive through `npm install` with no gate
// noticing. The allow-list below is deny.toml's, minus the two entries that only
// exist for Rust crates (LLVM-exception, Unicode-3.0).
//
// Production dependencies only (`--omit=dev` in spirit): a build tool's licence
// does not ship, and gating it would ban half the toolchain for no benefit to a
// user of the built app.
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");

const ALLOWED = new Set([
  "0BSD", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "BSL-1.0",
  "CC0-1.0", "MIT", "MIT-0", "MPL-2.0", "Unlicense", "Zlib",
]);

function orParts(expression) {
  return expression.split(/\s+OR\s+/i).map(part => part.trim().replace(/^\(|\)$/g, ""));
}

function walk(dir, seen, out) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const full = join(dir, entry.name);
    if (entry.isSymbolicLink()) continue;
    if (entry.isDirectory()) {
      if (entry.name.startsWith("@")) walk(full, seen, out);
      else if (entry.name !== ".bin") walk(full, seen, out);
    } else if (entry.name === "package.json") {
      if (seen.has(full)) continue;
      seen.add(full);
      let manifest;
      try {
        manifest = JSON.parse(readFileSync(full, "utf8"));
      } catch {
        continue;
      }
      out.push({ name: manifest.name ?? dir, license: manifest.license });
    }
  }
}

const found = [];
const seen = new Set();
const modules = join(root, "node_modules");
if (existsSync(modules)) walk(modules, seen, found);

const byName = new Map(found.map(pkg => [pkg.name, pkg]));

/**
 * The production closure: every workspace package's `dependencies`, transitively,
 * resolved through their own `dependencies`.
 *
 * Not all of node_modules — a build tool's licence does not ship, and gating it
 * would ban half the toolchain for no benefit to whoever runs the built app.
 * `npm ls --omit=dev` cannot answer this from the root, because the root has no
 * `dependencies` of its own and npm treats the workspaces separately.
 */
function productionClosure() {
  const queue = [];
  const enqueued = new Set();
  const push = (name) => {
    if (enqueued.has(name)) return;
    enqueued.add(name);
    const pkg = byName.get(name);
    if (pkg) queue.push(pkg);
  };

  for (const parent of ["packages", "apps"]) {
    if (!existsSync(join(root, parent))) continue;
    for (const name of readdirSync(join(root, parent))) {
      const manifest = join(root, parent, name, "package.json");
      if (!existsSync(manifest)) continue;
      for (const dep of Object.keys(JSON.parse(readFileSync(manifest, "utf8")).dependencies ?? {})) {
        push(dep);
      }
    }
  }

  const closure = [];
  const visited = new Set();
  while (queue.length) {
    const pkg = queue.shift();
    if (visited.has(pkg.name)) continue;
    visited.add(pkg.name);
    closure.push(pkg);
    // Walk *its* production dependencies. The manifest is in node_modules, which
    // is where the resolved copy lives.
    const manifest = join(modules, ...pkg.name.split("/"), "package.json");
    if (!existsSync(manifest)) continue;
    for (const dep of Object.keys(JSON.parse(readFileSync(manifest, "utf8")).dependencies ?? {})) {
      push(dep);
    }
  }
  return closure;
}

const problems = [];

// Undeclared is a documentation gap, not a copyleft finding — the risk this gate
// exists for is shipping copyleft code, and a missing field says nothing either
// way. So it is reported rather than failed on. Recorded so the next reader sees
// the gap instead of assuming the check covers it.
const undeclared = [];
for (const pkg of productionClosure()) {
  const raw = typeof pkg.license === "string" ? pkg.license : pkg.license?.type;
  if (!raw) {
    undeclared.push(pkg.name);
    continue;
  }
  const options = raw.includes(" OR ") ? orParts(raw) : [raw];
  if (!options.some(option => ALLOWED.has(option))) {
    problems.push(`${pkg.name}: ${raw}`);
  }
}

if (undeclared.length) {
  console.warn(`note: no licence field (not a violation): ${undeclared.join(", ")}`);
}
if (problems.length) {
  console.error(`disallowed licences:\n  ${problems.join("\n  ")}`);
  process.exit(1);
}
const closure = productionClosure();
console.log(`js licences ok (${closure.length} packages in the production closure)`);

// The workspace dependency graph, shared by `check.mjs` and `build-packages.mjs`.
//
// Extracted because both need the same answer to "in what order may these
// compile", and they used to answer it differently: check.mjs sorted
// topologically, while package.json's `build:packages` was a hand-written chain.
// The hand-written one had `transport-desktop` before `client`, and
// transport-desktop imports client — so `npm run build:packages` failed on a clean
// tree with TS2307 while `npm run check` on the same tree passed. One ordering,
// derived from the manifests, for both.
import { existsSync, readFileSync, readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
// Both scopes are workspace-local; a package belongs to whichever its name uses.
const scopes = ["@step-studio/", "@step-studio/"];

function readUnits() {
  const found = [];
  for (const parent of ["packages", "apps"]) {
    if (!existsSync(join(root, parent))) continue;
    for (const name of readdirSync(join(root, parent))) {
      const dir = `${parent}/${name}`;
      const manifest = join(root, dir, "package.json");
      if (!existsSync(manifest)) continue;
      const pkg = JSON.parse(readFileSync(manifest, "utf8"));
      const scope = scopes.find(prefix => pkg.name?.startsWith(prefix));
      if (scope === undefined) continue;
      const deps = Object.keys({ ...pkg.dependencies, ...pkg.devDependencies })
        .filter(dep => dep.startsWith(scope))
        .map(dep => dep.slice(scope.length));
      found.push({ key: pkg.name.slice(scope.length), dir, deps });
    }
  }
  return found.sort((left, right) => left.key.localeCompare(right.key));
}

export const units = readUnits();
export const byKey = new Map(units.map(unit => [unit.key, unit]));

/** Transitive internal dependencies, so building one package need not build all. */
export function closure(keys) {
  const needed = new Set();
  const visit = key => {
    if (needed.has(key) || !byKey.has(key)) return;
    needed.add(key);
    for (const dep of byKey.get(key).deps) visit(dep);
  };
  for (const key of keys) visit(key);
  return needed;
}

/**
 * Dependency levels inside a subset. Packages on the same level have no edges
 * between them, so they compile in parallel; each level waits for the one above.
 */
export function levels(keys) {
  const pending = new Map([...keys].map(key => [key, new Set(byKey.get(key).deps.filter(dep => keys.has(dep)))]));
  const ordered = [];
  while (pending.size) {
    const ready = [...pending].filter(([, deps]) => deps.size === 0).map(([key]) => key).sort();
    if (!ready.length) throw new Error(`dependency cycle between: ${[...pending.keys()].sort().join(", ")}`);
    ordered.push(ready);
    for (const key of ready) pending.delete(key);
    for (const deps of pending.values()) for (const key of ready) deps.delete(key);
  }
  return ordered;
}

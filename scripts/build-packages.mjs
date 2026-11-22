// Builds every workspace package in dependency order.
//
// Replaces the hand-written `build:packages` chain in package.json, which had
// transport-desktop ahead of client — a package it imports — and so failed on a
// clean tree while `npm run check` on the same tree passed. The order now comes
// from the manifests, shared with check.mjs through package-graph.mjs.
import { spawn } from "node:child_process";
import { availableParallelism } from "node:os";
import { join } from "node:path";
import { byKey, closure, levels, root } from "./package-graph.mjs";

const concurrency = Math.max(1, Math.min(4, availableParallelism() - 1));
const tscBin = join(root, "node_modules", "typescript", "bin", "tsc");

function buildStep(key) {
  const unit = byKey.get(key);
  // The renderer config is a solution file (files: [], references), so it needs -b.
  const args = key === "renderer" ? ["-b", unit.dir] : ["-p", `${unit.dir}/tsconfig.json`];
  return new Promise(done => {
    const child = spawn(process.execPath, [tscBin, ...args], { cwd: root, stdio: "inherit" });
    child.on("close", code => done({ key, ok: code === 0 }));
  });
}

async function pool(keys) {
  const results = [];
  for (let i = 0; i < keys.length; i += concurrency) {
    results.push(...await Promise.all(keys.slice(i, i + concurrency).map(buildStep)));
  }
  return results;
}

const ordered = levels(closure([...byKey.keys()]));
const failures = [];
for (const level of ordered) {
  const before = failures.length;
  for (const result of await pool(level)) if (!result.ok) failures.push(result.key);
  // A failed level would only repeat the same errors one level down.
  if (failures.length > before) break;
}

if (failures.length) {
  console.error(`\nbuild:packages failed for: ${failures.join(", ")}`);
  process.exit(1);
}
console.log(`\nbuilt ${[...byKey.keys()].length} packages in ${ordered.length} dependency levels`);

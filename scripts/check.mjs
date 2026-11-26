// Single entry point for "is this change sound?". Groups are addressable so the
// inner loop can run one slice instead of the whole workspace.
//
// Usage: node scripts/check.mjs [options]
//
//   (no options)      arch + selects + compile + core package suites + core renderer suites
//   --fast            arch + selects + compile only (no test run)
//   --all             run every package test file, not only scripts/core-suites.mjs
//   --production      also run the renderer / remote-host production build
//   --only <target>   run one group or one unit; a unit rebuilds its dependency
//                     closure first and then runs that unit's suites
//   --list            print groups and units
//
// Groups: arch, selects, types, packages, renderer, production
// Units:  studio-protocol, client, client-contract, studio-host, host-client-api,
//         transport-desktop, runtime-installer, platform, platform-win32,
//         testkit, renderer,
//
// A failing step prints only the tail of its output. The full log stays in the
// terminal scrollback, which keeps a 20-line type error readable.
import { spawn } from "node:child_process";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { availableParallelism } from "node:os";
import { join } from "node:path";
import { CORE_NODE_SUITES, CORE_RENDERER_SUITES } from "./core-suites.mjs";
import { byKey, closure, levels, root, units } from "./package-graph.mjs";

// The fork adds its own scope for Step-specific packages; both are workspace-local.
const scopes = ["@step-studio/", "@step-studio/"];
const tscBin = join(root, "node_modules", "typescript", "bin", "tsc");
const vitestBin = join(root, "node_modules", "vitest", "vitest.mjs");
const viteBin = join(root, "node_modules", "vite", "bin", "vite.js");
const tailLines = 40;
const concurrency = Math.max(1, Math.min(4, availableParallelism() - 1));

function buildStep(key) {
  const unit = byKey.get(key);
  // The renderer config is a solution file (files: [], references), so it needs -b.
  const args = key === "renderer" ? ["-b", unit.dir] : ["-p", `${unit.dir}/tsconfig.json`];
  return { name: `build:${key}`, command: process.execPath, args: [tscBin, ...args] };
}

function suiteFiles(key, all) {
  const unit = byKey.get(key);
  if (key === "renderer") return CORE_RENDERER_SUITES;
  if (all) {
    const dir = join(root, unit.dir, "dist", "test");
    if (!existsSync(dir)) return [];
    return readdirSync(dir)
      .filter(name => name.endsWith(".test.js"))
      .sort()
      .map(name => `${unit.dir}/dist/test/${name}`);
  }
  return CORE_NODE_SUITES.filter(suite => suite.startsWith(`${unit.dir}/`));
}

function testStep(key, all) {
  const unit = byKey.get(key);
  // Without file arguments Vitest runs the whole renderer suite, which is what
  // `--all` asks for; the default stays on the core list.
  if (key === "renderer") {
    return {
      name: "test:renderer",
      command: process.execPath,
      args: [vitestBin, "run", `--maxWorkers=${concurrency}`, ...(all ? [] : CORE_RENDERER_SUITES)],
      cwd: join(root, unit.dir),
    };
  }
  const files = suiteFiles(key, all);
  if (!files.length) return null;
  return { name: `test:${key}`, command: process.execPath, args: ["--test", `--test-concurrency=${concurrency}`, ...files] };
}

function runStep(step) {
  return new Promise(done => {
    const started = Date.now();
    const child = spawn(step.command, step.args, {
      cwd: step.cwd ?? root,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
      env: { ...process.env, FORCE_COLOR: "0" },
    });
    let output = "";
    child.stdout.on("data", chunk => (output += chunk));
    child.stderr.on("data", chunk => (output += chunk));
    child.on("error", error => done({ ...step, ok: false, ms: Date.now() - started, output: `${output}${error}` }));
    child.on("close", code => done({ ...step, ok: code === 0, ms: Date.now() - started, output }));
  });
}

function report(result) {
  const seconds = `${(result.ms / 1000).toFixed(1)}s`;
  if (result.ok) {
    console.log(`✓ ${result.name.padEnd(28)} ${seconds}`);
    return;
  }
  console.log(`✗ ${result.name.padEnd(28)} ${seconds}`);
  const lines = result.output.trimEnd().split("\n").slice(-tailLines);
  for (const line of lines) console.log(`    ${line}`);
  if (process.env.GITHUB_ACTIONS) {
    const text = lines
      .join("\n")
      // eslint-disable-next-line no-control-regex
      .replace(/\x1b\[[0-9;]*m/g, "")
      .replaceAll("%", "%25")
      .replaceAll("\r", "%0D")
      .replaceAll("\n", "%0A");
    console.log(`::error title=check failed: ${result.name}::${text}`);
  }
}

async function pool(steps, failures) {
  const queue = [...steps];
  const workers = Array.from({ length: Math.min(concurrency, queue.length) }, async () => {
    while (queue.length) {
      const result = await runStep(queue.shift());
      report(result);
      if (!result.ok) failures.push(result.name);
    }
  });
  await Promise.all(workers);
}

function usage() {
  console.log(`Usage: node scripts/check.mjs [--fast] [--all] [--production] [--only <target>] [--list]

  (no options)   arch + selects + compile + core suites
  --fast         arch + selects + compile only
  --all          every package test file, not only the core list
  --production   also build the renderer bundle and the remote host bundle
  --only <x>     one group (arch, selects, types, packages, renderer, production)
                 or one unit (studio-host, client, renderer, ...)
  --list         print groups and units`);
}

function list() {
  console.log("groups:");
  for (const group of ["arch", "selects", "types", "packages", "renderer", "production"]) console.log(`  ${group}`);
  console.log("units:");
  for (const unit of units) {
    const suite = unit.key === "renderer" ? CORE_RENDERER_SUITES.length : suiteFiles(unit.key, false).length;
    console.log(`  ${unit.key.padEnd(20)} ${unit.dir.padEnd(28)} suites=${suite}`);
  }
}

const argv = process.argv.slice(2);
let only = null;
const flags = new Set();
for (let index = 0; index < argv.length; index += 1) {
  const arg = argv[index];
  if (arg === "--only") only = argv[(index += 1)];
  else if (arg.startsWith("--only=")) only = arg.slice("--only=".length);
  else flags.add(arg);
}

if (flags.has("--help") || flags.has("-h")) {
  usage();
  process.exit(0);
}
if (flags.has("--list")) {
  list();
  process.exit(0);
}
if (argv.some(arg => arg === "--only" || arg.startsWith("--only=")) && !only) {
  console.error("--only needs a group or unit name");
  usage();
  process.exit(2);
}

const plan = { arch: false, selects: false, build: new Set(), test: new Set(), production: false };
const allUnits = () => units.map(unit => unit.key);

function useEverything() {
  plan.arch = true;
  plan.selects = true;
  for (const key of allUnits()) plan.build.add(key);
  for (const key of allUnits()) plan.test.add(key);
}

if (only) {
  if (byKey.has(only)) {
    for (const key of closure([only])) plan.build.add(key);
    plan.test.add(only);
  } else if (only === "arch") plan.arch = true;
  else if (only === "selects") plan.selects = true;
  else if (only === "types") for (const key of allUnits()) plan.build.add(key);
  else if (only === "packages") {
    for (const key of allUnits()) plan.build.add(key);
    for (const key of allUnits()) plan.test.add(key);
  } else if (only === "production") {
    for (const key of allUnits()) plan.build.add(key);
    plan.production = true;
  } else {
    console.error(`unknown target: ${only}`);
    usage();
    process.exit(2);
  }
} else if (flags.has("--fast")) {
  plan.arch = true;
  plan.selects = true;
  for (const key of allUnits()) plan.build.add(key);
} else {
  useEverything();
}
if (flags.has("--production")) plan.production = true;
// `--all` widens the suite list; with `--only <unit>` it widens only that unit.
if (flags.has("--all") && !only) plan.test = new Set(allUnits());

const failures = [];
const started = Date.now();

if (plan.arch) {
  const result = await runStep({
    name: "arch",
    command: process.execPath,
    args: ["--test", join(root, "scripts", "architecture.test.mjs")],
  });
  report(result);
  if (!result.ok) failures.push(result.name);
}

// Supply chain: no mutable action tags, and the JS licences stay on the
// allow-list. Both are pure source scans, so they belong with `arch`.
if (plan.arch) {
  for (const [name, script] of [
    ["action-pins", "mutable-action-tags.test.mjs"],
    ["packaged-csp", "packaged-csp.test.mjs"],
    ["js-licences", "check-js-licenses.mjs"],
  ]) {
    const result = await runStep({
      name,
      command: process.execPath,
      args: [join(root, "scripts", script)],
    });
    report(result);
    if (!result.ok) failures.push(result.name);
  }
}

// Lint. `error`-level rules must be clean; the staged `warn` rules are visible
// in the output but do not fail the step, which is what makes staging honest
// rather than a way of hiding them.
if (plan.arch) {
  const result = await runStep({
    name: "lint",
    command: process.execPath,
    // No `--max-warnings`: the staged `warn` rules are the documented backlog,
    // and failing on them would make the step red on day one. ESLint exits 0
    // with warnings present and 1 only on errors, which is the split we want.
    // `**/*` rather than the shell's `packages/*/src`: this runs without a shell,
    // so nothing expands the glob and ESLint would report "no files matching".
    args: [join(root, "node_modules", "eslint", "bin", "eslint.js"),
           "apps/renderer/src/**/*", "packages/*/src/**/*", "scripts/**/*",
           "--ext", ".ts,.tsx,.mjs,.js,.cjs"],
  });
  report(result);
  if (!result.ok) failures.push(result.name);
}

// Cross-process environment-variable contracts. A pure source scan — no build
// output needed — so it belongs with `arch`, not with the compile-dependent steps.
if (plan.arch) {
  const result = await runStep({
    name: "cross-process-contracts",
    command: process.execPath,
    args: ["--test", join(root, "scripts", "cross-process-contracts.test.mjs")],
  });
  report(result);
  if (!result.ok) failures.push(result.name);
}

if (plan.selects) {
  const result = await runStep({
    name: "selects",
    command: process.execPath,
    args: [join(root, "scripts", "check-renderer-selects.mjs")],
  });
  report(result);
  if (!result.ok) failures.push(result.name);
}

let buildFailed = false;
if (plan.build.size) {
  // Compiling a level needs the previous level's declarations on disk. A failed
  // level stops the chain: every later level would only repeat the same error.
  for (const level of levels(closure(plan.build))) {
    const before = failures.length;
    await pool(
      level.map(key => buildStep(key)),
      failures,
    );
    if (failures.length > before) {
      buildFailed = true;
      break;
    }
  }
}

// The generated contract. Regenerates `contracts/generated/` and fails if the
// checked-in copy moved, which is what makes `git diff --exit-code` on that
// directory a gate rather than a no-op. Runs after the builds because it reads
// the protocol from its built output.
// It needs the protocol's built output, so it runs only when this invocation
// compiled something or a previous one left the output on disk. Without that guard
// `--only arch` on a clean tree fails the same way the old ordering did.
const protocolBuilt = existsSync(join(root, "packages", "studio-protocol", "dist", "src", "index.js"));
if ((plan.arch || plan.types) && (plan.build.size > 0 || protocolBuilt)) {
  const result = await runStep({
    name: "contracts",
    command: process.execPath,
    args: ["--test", join(root, "test", "contracts.test.mjs")],
  });
  report(result);
  if (!result.ok) failures.push(result.name);
}

const suites = [];
for (const key of plan.test) {
  const step = testStep(key, flags.has("--all"));
  if (step) suites.push(step);
}
if (plan.renderer && !suites.some(step => step.name === "test:renderer")) {
  const step = testStep("renderer", false);
  if (step) suites.push(step);
}
if (suites.length && !buildFailed) await pool(suites, failures);

if (plan.production) {
  // The renderer bundle only. The two Electron steps that used to sit beside it
  // (the SSH remote host and the sandboxed preload) went with `apps/desktop`.
  const steps = [
    { name: "bundle:renderer", command: process.execPath, args: [viteBin, "build"], cwd: join(root, "apps", "renderer") },
  ];
  await pool(steps, failures);
  // After the bundle, not beside it: this asserts the built index.html actually
  // carries the policy, which is the step that fails open. The `arch`-group
  // `packaged-csp` test reads the source constant, so a plugin that stops
  // injecting — a lost literal `<head>`, the `includes("Content-Security-
  // Policy")` early return tripping, or the plugin dropped from the array —
  // would ship an app with no CSP at all while the source test stayed green.
  const artifact = await runStep({
    name: "packaged-csp-artifact",
    command: process.execPath,
    args: [join(root, "scripts", "packaged-csp-artifact.test.mjs")],
  });
  report(artifact);
  if (!artifact.ok) failures.push(artifact.name);
}

console.log("");
if (failures.length) {
  console.log(`${failures.length} step(s) failed: ${failures.join(", ")}`);
  process.exit(1);
}
console.log(`all checks passed in ${((Date.now() - started) / 1000).toFixed(1)}s`);

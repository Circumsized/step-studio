/**
 * Generate `contracts/generated/protocol-names.json`.
 *
 * The single source of truth for operation names is `packages/studio-protocol`.
 * The shell's dispatch tables in `apps/desktop-tauri/src-tauri/src/pump.rs` are a
 * second list that must agree with it, and two lists that must agree drift —
 * silently, because nothing fails when a name is added to one and not the other.
 *
 * So: emit both into one checked-in artifact. A test regenerates it and compares,
 * which makes `git diff --exit-code contracts/generated/` a real gate rather than
 * a no-op on a directory that does not exist.
 *
 * Deterministic by construction — sorted, no timestamps — so a rerun on an
 * unchanged tree produces a byte-identical file.
 *
 * Run after `npm run build`: it imports the protocol from its built output,
 * because `WORKBENCH_OPERATION_KINDS` is assembled from a dozen per-domain
 * constants and parsing that out of the source is a text-scraping exercise that
 * breaks the first time someone reorders an import.
 */

import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import * as protocol from "../packages/studio-protocol/dist/src/index.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const output = join(root, "contracts", "generated", "protocol-names.json");

/**
 * The keys of one interface in a `.d.ts`.
 *
 * Read from the declaration file rather than imported, because `operations.ts` is
 * types-only and compiles to `export {}` — there is no runtime value to import.
 * Quoted member keys are what `tsc` emits for a name containing a dot, which is
 * every one of these, so the shape is stable.
 */
function interfaceKeys(declarations, name) {
  const start = declarations.indexOf(`interface ${name}`);
  if (start < 0) throw new Error(`${name} is not declared in operations.d.ts`);
  const body = declarations.slice(start, declarations.indexOf("\n}", start));
  const keys = [...body.matchAll(/^\s+"([^"]+)"\??:/gm)].map(m => m[1]);
  if (keys.length === 0) throw new Error(`${name} parsed as empty`);
  return keys;
}



const declarations = readFileSync(
  join(root, "packages", "client-contract", "dist", "src", "operations.d.ts"),
  "utf8",
);

/** Every name the facade can be asked for: queries and commands both. */
// `CommandInputMap` is an intersection of four maps, and two of them are *mapped*
// types — `[K in EvaluationOperation["kind"]]: ...` — whose keys are not literals
// and cannot be scraped from the declaration. Their keys are exactly the
// protocol's `*_KINDS` constants, which are exported at runtime. So: scrape the
// literal maps, and take the mapped ones from the constants. Reading only three
// of the four silently drops a third of the command surface, which is precisely
// the drift this artifact exists to catch.
const literalFacade = [
  ...interfaceKeys(declarations, "QueryInputMap"),
  ...interfaceKeys(declarations, "QueryResultMap"),
  ...interfaceKeys(declarations, "CoreCommandInputMap"),
  ...interfaceKeys(declarations, "CoreCommandResultMap"),
  // The  family lives here, outside every mapped type — which is why
  // dropping this one call reported  as an undeclared name the shell
  // serves.
  ...interfaceKeys(declarations, "RuntimeCommandInputMap"),
];
const kindConstantNames = Object.keys(protocol).filter(name => name.endsWith("_KINDS"));
const mappedFacade = kindConstantNames.flatMap(name => protocol[name]);
const facadeNames = [...new Set([...literalFacade, ...mappedFacade])].sort();

/** The Rust dispatch tables, read as text — there is no Rust codegen here. */
const pump = readFileSync(
  join(root, "apps", "desktop-tauri", "src-tauri", "src", "pump.rs"),
  "utf8",
);

/** One `const NAME: &[(&str, &str)] = &[...]` table, as `[name, grade]` pairs. */
function rustTable(source, name) {
  const start = source.indexOf(`const ${name}: &[(&str, &str)] = &[`);
  if (start < 0) throw new Error(`${name} is not declared`);
  const body = source.slice(start, source.indexOf("\n];", start));
  const pairs = [...body.matchAll(/\("([^"]+)", "([^"]+)"\)/g)].map(m => [m[1], m[2]]);
  if (pairs.length === 0) throw new Error(`${name} parsed as empty`);
  return Object.fromEntries(pairs);
}

/**
 * The runtime adapter's capability ids, as `[id, grade]` pairs.
 *
 * A three-field table rather than two, and read separately because these are the
 * names the *adapter* serves — which is where a rename's target lives. `usage.get`
 * renames to `usage.panel`, and `usage.panel` is an adapter capability, not a
 * protocol operation: checking only the protocol reported it as undeclared.
 */
function runtimeCapabilities() {
  const source = readFileSync(
    join(root, "crates", "step-studio-runtime", "src", "capabilities.rs"),
    "utf8",
  );
  const start = source.indexOf("const CAPABILITIES: &[(&str, &str, &str)] = &[");
  if (start < 0) throw new Error("CAPABILITIES is not declared in capabilities.rs");
  const body = source.slice(start, source.indexOf("\n];", start));
  // `\s*` after the paren too: the entries whose third field is prose are
  // written across lines, and a regex matching only the one-line form
  // silently dropped exactly those — `usage.panel` among them.
  const pairs = [...body.matchAll(/\(\s*"([^"]+)",\s*"([^"]+)"/g)].map(m => [m[1], m[2]]);
  if (pairs.length === 0) throw new Error("CAPABILITIES parsed as empty");
  return Object.fromEntries(pairs);
}
const contract = {
  generatedFrom:
    "packages/studio-protocol (*_KINDS) + packages/client-contract (operations.d.ts)",
  // Every operation a Runtime can be asked for.
  operations: [...new Set(kindConstantNames.flatMap(name => protocol[name]))].sort(),
  // Every name the facade can be asked for. `capabilities.get` is one of these
  // and not a workbench operation, which is why both lists are here: checking
  // only the first reported a name the shell legitimately serves.
  facade: facadeNames,
  // What this shell answers itself, without an adapter round trip.
  shellLocal: rustTable(pump, "SHELL_LOCAL"),
  // What it forwards to the adapter under the adapter's own spelling.
  adapterNative: rustTable(pump, "ADAPTER_NATIVE"),
  // Facade spellings that resolve to something else, and what they resolve to.
  renamed: rustTable(pump, "RENAMED"),
  // What the adapter itself serves, with the grade it declares.
  runtimeCapabilities: runtimeCapabilities(),
};

mkdirSync(dirname(output), { recursive: true });
writeFileSync(output, `${JSON.stringify(contract, null, 2)}\n`);
console.log(
  `wrote contracts/generated/protocol-names.json ` +
    `(${contract.operations.length} operations, ${Object.keys(contract.shellLocal).length} shell-local)`,
);

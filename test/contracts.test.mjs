/**
 * The `contracts/generated/` gate.
 *
 * The checked-in artifact is regenerated on every run and compared. If it
 * differs, the tree has drifted — either the protocol gained an operation, or the
 * shell's dispatch tables changed — and the diff is the record of what. That is
 * what makes `git diff --exit-code contracts/generated/` after `cargo test` a
 * gate rather than a no-op on a directory that does not exist.
 *
 * The second half is the reason the artifact exists at all: every name the shell
 * claims to serve must be one the protocol declares. Two lists that must agree
 * drift silently otherwise — nothing fails when a name is added to one and not
 * the other, and the first symptom is a renderer offering an action the shell
 * rejects at runtime.
 */

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, test } from "node:test";

const root = join(import.meta.dirname, "..");
const artifact = join(root, "contracts", "generated", "protocol-names.json");

/** Regenerate, and report whether the checked-in copy changed. */
function regenerate() {
  const before = readFileSync(artifact, "utf8");
  execFileSync(process.execPath, [join(root, "scripts", "generate-contracts.mjs")], {
    cwd: root,
    stdio: "pipe",
  });
  return readFileSync(artifact, "utf8") !== before;
}

describe("the generated contract", () => {
  test("is what regenerating produces", () => {
    assert.equal(
      regenerate(),
      false,
      "contracts/generated/protocol-names.json is stale; run `node scripts/generate-contracts.mjs` and commit it",
    );
  });

  test("agrees with the protocol on every name the shell serves", () => {
    const contract = JSON.parse(readFileSync(artifact, "utf8"));
    // Two different surfaces, and checking only one of them is a false alarm:
    // `capabilities.get` is a facade query the protocol's operation kinds never
    // name, and the shell legitimately serves it. So a name has to be declared by
    // *either*.
    // Three surfaces a name can legitimately come from: the protocol's operation
    // kinds, the facade's query/command names, and the runtime adapter's own
    // capability ids. A rename's target is usually the third — `usage.get` →
    // `usage.panel`, and `usage.panel` is an adapter capability, not a protocol
    // operation. Checking only the first two reported it as undeclared.
    const declared = new Set([
      ...contract.operations,
      ...contract.facade,
      ...Object.keys(contract.runtimeCapabilities),
    ]);
    const served = new Set([
      ...Object.keys(contract.shellLocal),
      ...Object.keys(contract.adapterNative),
      ...Object.keys(contract.renamed),
    ]);
    // The facade spellings are what the renderer calls, and they are declared in
    // their own right — so only the *target* of a rename has to be a known
    // operation, not the spelling itself.
    const targets = Object.values(contract.renamed);
    for (const name of [...served].filter(name => !targets.includes(name))) {
      assert.ok(
        declared.has(name),
        `${name} is served by the shell but neither the protocol nor the facade declares it`,
      );
    }
    for (const target of targets) {
      assert.ok(
        declared.has(target) || Object.keys(contract.adapterNative).includes(target),
        `${target} is the target of a rename but is neither a protocol operation nor an adapter-native one`,
      );
    }
    // And both surfaces are real, so a parse that silently returned one of them
    // empty cannot pass.
    assert.ok(contract.facade.length > 100, "the facade surface was read");
    assert.ok(
      contract.facade.includes("capabilities.get"),
      "the facade surface includes the shell-local queries",
    );
    // And the other direction: an operation nothing serves is not a bug, but a
    // grade of `stable` on a name the shell refuses would be. The manifest test
    // in pump.rs covers that; this covers the shape.
    assert.ok(contract.operations.length > 100, "the protocol declares a real surface");
  });
});

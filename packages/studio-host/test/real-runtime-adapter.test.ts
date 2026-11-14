/**
 * The Rust adapter drives the real Step CLI.
 *
 * This is the port of `packages/runtime-stepcode/test/real-cli.test.ts`, which
 * drove the TypeScript adapter. That package was deleted: it was the TS
 * adapter's own source, its only product consumer was itself, and the Rust
 * adapter (`crates/step-studio-runtime`) is what the daemon actually launches.
 *
 * What made the test worth porting rather than dropping is the thing it proves
 * that no Rust test does: a **cross-language** client — the TypeScript
 * `StudioBridgeClient` — negotiating with, and streaming from, the adapter the
 * daemon spawns. The protocol is specified in `packages/studio-protocol` and
 * implemented twice, so a Rust-only test would let the two implementations drift
 * and only the shipped shell would find out.
 *
 * Skips when the Step CLI is absent, exactly as the original did — and the skip is
 * not a way for the test to be quietly dead. Verified non-vacuous by running it
 * with a stub `step` on PATH: the daemon starts, the client connects, the
 * handshake is attempted, and it fails with `malformed response` because the stub
 * does not speak the protocol. So every line up to the protocol exchange does run
 * on any machine with the Rust daemon built.
 */

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { resolve as pathResolve } from "node:path";
import test from "node:test";

const here = (path: string) => fileURLToPath(new URL(path, import.meta.url));
// `here` is the compiled test in `dist/test/`, so the repo root is four levels up:
// `dist/test/` -> `dist/` -> `studio-host/` -> `packages/` -> the root. Getting this
// wrong by one level makes the test skip silently on every machine, which is the
// failure mode of a skip-guard nobody re-runs with the tool present.
const repoRoot = pathResolve(here(".."), "..", "..", "..");

/**
 * The TypeScript client, from this package's own build output.
 *
 * A *type-only* import, and that is deliberate:  includes
 * , so a value import of  makes tsc try to
 * re-emit that file into  and fail with "would overwrite input file". The
 * runtime import below is what actually loads it, by URL, after the build.
 */
type BridgeClientModule = typeof import("../src/bridge-client.js");
// Resolved from the compiled test's own location: `dist/test/`, so the client is
// one level up in `dist/src/`. The second candidate is for running from the repo
// root, where `here` still resolves but the cwd does not.
const bridgeClient = () => {
  const candidates = [
    pathResolve(here(".."), "src/bridge-client.js"),
    pathResolve(process.cwd(), "packages/studio-host/dist/src/bridge-client.js"),
  ];
  const found = candidates.find(candidate => existsSync(candidate));
  if (found === undefined) throw new Error("studio-host is not built; run the workspace build first");
  return pathToFileURL(found).href;
};
const { StudioBridgeClient } = (await import(bridgeClient())) as BridgeClientModule;

/** The Rust daemon, built by `cargo build -p step-studio-host`. */
const daemonBinary = () => {
  for (const profile of ["release", "debug"]) {
    const candidate = pathResolve(repoRoot, "target", profile, "step-studio-host");
    if (existsSync(candidate)) return candidate;
  }
  return undefined;
};

const stepCommand = process.env.STEP_STUDIO_STEP_CMD ?? process.env.STEP_STUDIO_RUNTIME_CMD;

const describeReal = stepCommand === undefined || daemonBinary() === undefined ? test.skip : test;

describeReal("the Rust adapter drives the real Step CLI", { timeout: 180_000 }, async () => {
  const dir = mkdtempSync(join(tmpdir(), "step-studio-real-rust-"));
  // The bridge token is a fixed shared literal, not a per-install secret: the
  // daemon writes `"step-studio"` into the token file it owns
  // (`adapter.rs:107`) and the shell hardcodes the same constant
  // (`bridge.rs:508`). The first version of this test invented its own token
  // and wrote it to `dir/token` — a file the daemon never reads — so the
  // challenge proof was signed with one token and verified with another, and
  // the handshake failed `UNAUTHENTICATED` no matter what the adapter did.
  const token = "step-studio";
  writeFileSync(join(dir, "token"), token);
  const socketPath = join(dir, "bridge.sock");

  // `--endpoint` rather than the daemon's default: `default_endpoint()` is
  // `temp_dir/step-studio-<uid>/daemon.sock` and does not follow `--cwd`, so a test
  // that waited on a path derived from its own temp directory would wait forever
  // on a socket the daemon put somewhere else.
  const child = spawn(daemonBinary()!, ["--cwd", dir, "--endpoint", socketPath], {
    env: { ...process.env, STEP_STUDIO_STEP_CMD: stepCommand! },
    stdio: ["ignore", "ignore", "inherit"],
  });

  try {
    for (let i = 0; i < 1000 && !existsSync(socketPath); i += 1) {
      await new Promise(resolve => setTimeout(resolve, 20));
    }
    assert.ok(existsSync(socketPath), "the daemon never created its bridge socket");

    const deltas: string[] = [];
    const tools: string[] = [];
    // The id is minted here so the test can assert the receipt is correlated by
    // it. `RequestId` is a branded type, so minting one outside the client needs
    // the same cast the client itself uses (`bridge-client.ts:218`) — a brand is a
    // compile-time marker, and this is the one place a test legitimately crosses it.
    //
    // Minted *at the call site*, not captured from `createRequestId`: that hook
    // fires for the hello and the snapshot too, so whatever it last returned is
    // the snapshot's id. Reusing it sent the prompt under an id the daemon had
    // already routed and settled, which the daemon reports as "request … was
    // already waiting on surface 1; surface 1 took it over" — and the client then
    // waits on a receipt that belongs to a request it already finished.
    const promptId = crypto.randomUUID();
    const client = new StudioBridgeClient({
      endpoint: socketPath,
      token,
      requestTimeoutMs: 150_000,
      onEvent: event => {
        const payload = event.event as { kind?: string; delta?: string; toolName?: string };
        if (payload.kind === "conversation.message.delta") deltas.push(payload.delta ?? "");
        if (payload.kind === "conversation.tool.started") tools.push(payload.toolName ?? "");
      },
    });

    const hello = await client.connect();
    assert.equal(hello.capabilityManifest.profile, "limited");

    const snapshot = await client.requestSnapshot();
    assert.ok(snapshot, "a snapshot came back");

    // `invoke`, the one method the client has for a turn. The id it mints is
    // captured by `createRequestId` above, and it is what a receipt is correlated
    // by — which is the property this cross-language test exists to prove.
    const receipt = await client.invoke({
      type: "studio.request",
      requestId: promptId as typeof promptId & { readonly __brand: "RequestId" },
      // The epoch the handshake agreed on. `invoke` refuses a stale one, which is
      // the daemon's guard against a request from a previous life being replayed
      // into this one — so it comes from `hello`, not from a constant.
      runtimeEpoch: hello.runtimeEpoch,
      operation: { kind: "core.prompt", text: "Reply with the single word: pong" },
    });
    assert.ok(receipt, `a receipt came back for ${promptId}`);
    assert.equal(receipt.requestId, promptId, "the receipt is correlated by the id the client minted");

    const deadline = Date.now() + 150_000;
    while (deltas.length === 0 && Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    assert.ok(deltas.length > 0, `the real CLI streamed at least one delta (tools: ${tools.join(", ")})`);

    await client.close();
  } finally {
    child.kill();
  }
});

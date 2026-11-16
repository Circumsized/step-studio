import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, sign } from "node:crypto";
import { test } from "node:test";
import { fetchUpdateIndex } from "../src/index.js";

// The rollback watermark: `fetchUpdateIndex` is the only function in the
// repository that refuses an index older than the one this machine already
// installed, and until this file it had no test at all. The property lives in
// `signing.rs` on the Rust side, but that module has no production caller, so
// the check that actually guards an update was untested.
const pair = generateKeyPairSync("ed25519");
const trustedKey = pair.publicKey.export({ type: "spki", format: "pem" });

const index = (sequence: number) => ({
  schema: 1,
  sequence,
  generatedAt: "2026-09-17T00:00:00Z",
  repo: "owner/repo",
  app: {
    version: "1.0.0",
    setup: {
      asset: "setup.exe",
      url: "https://github.com/owner/repo/releases/download/v1/setup.exe",
      size: 3,
      sha256: createHash("sha256").update("abc").digest("hex"),
    },
  },
  runtime: {
    runtimeVersion: "1.0.0",
    channel: "stable",
    platform: "win32-x64",
    entrypoint: "omp.exe",
    minAppVersion: "0.0.0",
    studioProtocol: { min: 1, max: 1 },
    files: [{
      name: "omp.exe",
      url: "https://github.com/owner/repo/releases/download/v1/omp.exe",
      size: 3,
      sha256: createHash("sha256").update("abc").digest("hex"),
    }],
  },
});

/** Serve a signed index, or serve bytes that do not match its signature. */
function serve(payload: Buffer, tamper: boolean) {
  const signature = {
    algorithm: "ed25519" as const,
    keyId: "test",
    // A mismatching digest is the tamper: the payload the caller signed and the
    // payload served stop being the same bytes.
    payloadSha256: createHash("sha256")
      .update(tamper ? Buffer.from("something else") : payload)
      .digest("hex"),
    signature: sign(null, payload, pair.privateKey).toString("base64url"),
  };
  // Typed as `typeof fetch` because that is what `fetchUpdateIndex` takes: a
  // narrower parameter type is a compile error, not a runtime surprise.
  // `Parameters<typeof fetch>[0]` rather than `RequestInfo`: the workspace
  // compiles against `lib: ["ES2022"]`, so the DOM name does not exist here.
  const fetcher: typeof fetch = async (input: Parameters<typeof fetch>[0]) => {
    const text = String(input).endsWith(".sig.json")
      ? JSON.stringify(signature)
      : payload.toString("utf8");
    return new Response(text, { status: 200 });
  };
  return fetcher;
}

const base = { repo: "owner/repo", mirrorPrefix: "", trustedKeys: { test: trustedKey } };

test("an index at or above the local watermark is accepted", async () => {
  const payload = Buffer.from(JSON.stringify(index(7)));
  // The same release again must stay possible: a re-check is not a rollback.
  assert.equal((await fetchUpdateIndex({ ...base, lastSequence: 7, fetcher: serve(payload, false) })).sequence, 7);
  assert.equal((await fetchUpdateIndex({ ...base, lastSequence: 6, fetcher: serve(payload, false) })).sequence, 7);
});

test("an older signed index is refused, because a valid signature is not a fresh one", async () => {
  // Signed by the trusted key, well-formed, and still refused: this is the
  // rollback attack, and the watermark is what stops it.
  const payload = Buffer.from(JSON.stringify(index(7)));
  await assert.rejects(
    fetchUpdateIndex({ ...base, lastSequence: 8, fetcher: serve(payload, false) }),
    /older than local watermark 8/,
  );
});

test("an index whose payload does not match its signed digest is refused", async () => {
  const payload = Buffer.from(JSON.stringify(index(7)));
  await assert.rejects(
    fetchUpdateIndex({ ...base, lastSequence: 1, fetcher: serve(payload, true) }),
    /signature verification failed/,
  );
});

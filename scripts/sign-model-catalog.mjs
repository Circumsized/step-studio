/**
 * Sign `catalog/model-catalog-v1.json` so installed clients can authenticate a
 * catalog update.
 *
 * `packages/host-client-api/src/model-adaptation.ts` verifies this signature
 * against the public keys baked into the binary. The private key never enters
 * this repository: it is the GitHub Actions Release environment secret, exactly
 * as `packaging/keys/README.md` records for the Runtime signing key. Run this
 * with that secret in the environment, then commit both files together — a
 * catalog whose `.sig.json` is missing or stale makes every client refuse its
 * update, which is the intended failure.
 *
 *   STEP_STUDIO_RUNTIME_SIGNING_KEY=/path/to/key.pem \
 *   STEP_STUDIO_RUNTIME_SIGNING_KEY_ID=step-studio-release-2026b \
 *   node scripts/sign-model-catalog.mjs
 */
import { sign } from "node:crypto";
import { readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const catalogPath = join(root, "catalog", "model-catalog-v1.json");

const keyPath = process.env.STEP_STUDIO_RUNTIME_SIGNING_KEY;
if (!keyPath) throw new Error("STEP_STUDIO_RUNTIME_SIGNING_KEY is required");
const signingKey = await readFile(keyPath);
const keyId = process.env.STEP_STUDIO_RUNTIME_SIGNING_KEY_ID
  ?? JSON.parse(await readFile(join(root, "packaging", "keys", "trusted-keys.json"), "utf8")).activeKeyId;

const bytes = await readFile(catalogPath);
const envelope = { schema: 1, keyId, algorithm: "ed25519", value: sign(null, bytes, signingKey).toString("base64url") };
const target = `${catalogPath}.sig.json`;
await writeFile(target, `${JSON.stringify(envelope, null, 2)}\n`, "utf8");
console.log(`Signed ${resolve(root, "catalog", "model-catalog-v1.json")} with ${keyId}`);

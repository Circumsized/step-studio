import { createPublicKey, verify } from "node:crypto";
import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import {
  MODEL_PARAMETER_FIELDS, mergeModelParameters, parseModelThinkingConfig, sameModelParameter,
  type ModelAdaptationInput, type ModelAdaptationResult, type ModelCatalogDocument,
  type ModelAdaptationCandidate,
  type ModelCatalogStatus, type ModelCatalogRefreshResult, type ModelParameterValues,
  type ModelParameterSources, type ModelParameterOrigin,
} from "@step-studio/client-contract";
import { BUILTIN_MODEL_CATALOG } from "./model-catalog.generated.js";

export const MODEL_CATALOG_URL = "https://raw.githubusercontent.com/Circumsized/step-studio/main/catalog/model-catalog-v1.json";
export const MODEL_CATALOG_SIGNATURE_URL = `${MODEL_CATALOG_URL}.sig.json`;

/**
 * Ed25519 public keys a catalog update must be signed with, keyed by the `keyId`
 * the signature envelope carries. Mirrors `packaging/keys/trusted-keys.json`:
 * only public keys live in this repository, the private key is a GitHub Actions
 * Release environment secret, and rotation adds a row here rather than editing
 * the existing ones.
 *
 * The trust anchor for an installed client is this table baked into its own
 * binary — not the repository. A push that rewrites these files reaches users
 * only in the next app release, which is the same anchor the old
 * `MODEL_CATALOG_SHA256` constant had. That is why signing works here even
 * though the catalog itself is fetched from this repository's `main`.
 */
export const MODEL_CATALOG_PUBLIC_KEYS: Readonly<Record<string, string>> = Object.freeze({
  "step-studio-release-2026a": `-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAXap1D/Nd2WfZ8W2205AVgHh5vhGT7CpwicFi5QP3U9w=
-----END PUBLIC KEY-----
`,
  "step-studio-release-2026b": `-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAnULFVtKQXCr/6RErL1JAUB2AJx6zQcticWFpfqzBOs8=
-----END PUBLIC KEY-----
`,
});

/** A catalog is model metadata, not a payload; 1 MiB is generous and matches the Runtime's bound. */
export const MODEL_CATALOG_MAX_BYTES = 1024 * 1024;

export type CatalogSignatureEnvelope = { algorithm: string; keyId: string; value: string };
export type CatalogSignatureVerifier = (signature: CatalogSignatureEnvelope, payload: Uint8Array) => boolean;

/** The default verifier: any key in `MODEL_CATALOG_PUBLIC_KEYS`, Ed25519, nothing else. */
export function createCatalogSignatureVerifier(keys: Readonly<Record<string, string>> = MODEL_CATALOG_PUBLIC_KEYS): CatalogSignatureVerifier {
  return (signature, payload) => {
    const pem = keys[signature.keyId];
    if (pem === undefined || signature.algorithm !== "ed25519") return false;
    try {
      const key = createPublicKey(pem);
      if (key.asymmetricKeyType !== "ed25519") return false;
      return verify(null, payload, key, Buffer.from(signature.value, "base64url"));
    } catch {
      return false;
    }
  };
}

/**
 * Refuse a catalog update whose bytes are not signed by a trusted key.
 *
 * This is the control that used to be `MODEL_CATALOG_SHA256` byte equality, and
 * the reason it was wrong is worth recording: equality can only ever accept the
 * *same* catalog, so the update path was impossible by construction and every
 * client built before a catalog change was permanently stuck on `digest
 * mismatch`. A signature accepts *new* bytes while still refusing bytes the key
 * holder did not produce — which is what a mutable `main` URL needs, because a
 * push by anyone without the key yields a catalog the next client rejects.
 */
export function verifyCatalogSignature(catalogBytes: Uint8Array, signatureText: string, verifySignature: CatalogSignatureVerifier = createCatalogSignatureVerifier()): void {
  let envelope: unknown;
  try {
    envelope = JSON.parse(signatureText);
  } catch {
    throw new Error("Model catalog signature is not JSON");
  }
  const signature = envelope as Partial<CatalogSignatureEnvelope> | null;
  if (!signature || typeof signature.algorithm !== "string" || typeof signature.keyId !== "string" || typeof signature.value !== "string") {
    throw new Error("Model catalog signature envelope is malformed");
  }
  // The raw bytes, never a decoded-and-re-encoded string: `TextDecoder` strips a
  // BOM and replaces invalid sequences with U+FFFD, so a round trip is not the
  // identity and the signer (which signs the file's bytes) and this verifier
  // would silently disagree.
  if (!verifySignature(signature as CatalogSignatureEnvelope, catalogBytes)) {
    throw new Error(`Model catalog signature does not verify against key ${signature.keyId}; refusing the update`);
  }
}

/** Read a response body, refusing it early rather than after buffering it all. */
async function boundedBytes(response: Response, limit: number): Promise<Uint8Array> {
  if (!response.ok) throw new Error(`Model catalog update failed: HTTP ${response.status}`);
  const declared = Number(response.headers.get("content-length") ?? "");
  if (Number.isFinite(declared) && declared > limit) throw new Error(`Model catalog update is too large: ${declared} bytes`);
  if (!response.body) throw new Error("Empty model catalog response");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.length;
      if (total > limit) throw new Error("Model catalog update exceeds the size limit");
      chunks.push(value);
    }
  } finally {
    await reader.cancel().catch(() => undefined);
    reader.releaseLock();
  }
  const merged = new Uint8Array(total);
  let at = 0;
  for (const chunk of chunks) { merged.set(chunk, at); at += chunk.length; }
  return merged;
}

/**
 * SHA-256 of the catalog this build ships with.
 *
 * The catalog is a repository file, so its digest is a repository fact: changing
 * the catalog without changing this constant is a commit that lies. The gate
 * `npm run models:catalog:check` fails when the two disagree.
 *
 * Build-time self-check only. It used to gate `refresh()` as well, and that made
 * the update path impossible by construction: a refresh exists to accept a *new*
 * catalog, and byte equality can only ever accept the *same* one. Every client
 * built before a catalog change was permanently stuck on `digest mismatch` —
 * including a legitimate v2, which is exactly what the refresh test reproduced.
 *
 * Authenticity at refresh time is `verifyCatalogSignature`'s job, not this
 * constant's. See `MODEL_CATALOG_PUBLIC_KEYS` for why a signature works here
 * even though the catalog is fetched from this repository.
 */
export const MODEL_CATALOG_SHA256 = "e65289617c95a775e62715f0960b9a32b977a581e2a1cd18c0a9b6a3c6578a89";

export function normalizeModelName(name: string): string {
  return name.normalize("NFKC").toLowerCase().trim().replace(/[\s._]+/g, "-").replace(/-+/g, "-");
}

const catalogIndexes = new WeakMap<ModelCatalogDocument, ReturnType<typeof buildCatalogIndex>>();
function buildCatalogIndex(catalog: ModelCatalogDocument) {
  const exact = new Map<string, ModelAdaptationCandidate[]>();
  const ids = new Map<string, ModelAdaptationCandidate[]>();
  const names = new Map<string, ModelAdaptationCandidate[]>();
  const apis = new Map<string, Array<{ row: ModelAdaptationCandidate; name: string }>>();
  const add = (map: Map<string, ModelAdaptationCandidate[]>, key: string, row: ModelAdaptationCandidate) => { const list = map.get(key) ?? []; list.push(row); map.set(key, list); };
  for (const row of catalog.entries) {
    add(exact, JSON.stringify([row.api, row.provider, row.id]), row);
    add(ids, JSON.stringify([row.api, row.id]), row);
    for (const name of new Set([row.id, row.name, ...(row.aliases ?? [])].map(normalizeModelName))) add(names, JSON.stringify([row.api, name]), row);
    const list = apis.get(row.api) ?? []; list.push({ row, name: normalizeModelName(row.name) }); apis.set(row.api, list);
  }
  return { exact, ids, names, apis };
}

export function resolveModelAdaptation(catalog: ModelCatalogDocument, input: ModelAdaptationInput): ModelAdaptationResult {
  let index = catalogIndexes.get(catalog);
  if (!index) { index = buildCatalogIndex(catalog); catalogIndexes.set(catalog, index); }
  const exact = index.exact.get(JSON.stringify([input.api, input.providerId, input.modelId])) ?? [];
  const sameId = index.ids.get(JSON.stringify([input.api, input.modelId])) ?? [];
  const names = new Set([input.modelId, input.name ?? ""].filter(Boolean).map(normalizeModelName));
  const named = [...new Set([...names].flatMap(name => index!.names.get(JSON.stringify([input.api, name])) ?? []))];
  const definite = exact.length ? exact : sameId.length ? sameId : named;
  const grouped = definite.filter((row, i) => definite.findIndex(other => sameModelParameter(other.values, row.values) && other.limitation === row.limitation) === i);
  const nearby = definite.length ? [] : (index.apis.get(input.api) ?? []).filter(({ name: normalized }) => [...names].some(name => name.length >= 5 && (normalized.startsWith(`${name}-`) || name.startsWith(`${normalized}-`)))).slice(0, 12).map(item => item.row);
  const candidates = grouped.length ? grouped : nearby;
  const status = grouped.length === 1 ? "matched" : candidates.length ? "ambiguous" : "none";
  const recommended = status === "matched" ? candidates[0]!.values : {};
  const values = mergeModelParameters(recommended, input.endpoint ?? {});
  const sources: Record<string, ModelParameterOrigin> = {};
  for (const field of MODEL_PARAMETER_FIELDS) {
    if (values[field] === undefined) continue;
    sources[field] = input.endpoint?.[field] !== undefined
      ? { source: "endpoint" }
      : { source: "catalog", catalogVersion: catalog.version, catalogModel: candidates[0]?.key ?? "" };
  }
  return { catalogVersion: catalog.version, status, candidates, values, sources };
}

/** The only validation boundary for downloaded catalog data. */
export function decodeModelCatalog(value: unknown): ModelCatalogDocument {
  const doc = value as ModelCatalogDocument;
  if (!doc || doc.schemaVersion !== 1 || typeof doc.version !== "string" || typeof doc.upstreamCommit !== "string" || !Array.isArray(doc.entries)) throw new Error("Unsupported model catalog format");
  for (const row of doc.entries) {
    if (![row.key, row.provider, row.id, row.name, row.api].every(v => typeof v === "string") || !row.values || (row.aliases !== undefined && (!Array.isArray(row.aliases) || !row.aliases.every((alias: unknown) => typeof alias === "string")))) throw new Error("Invalid model catalog entry");
    const values = row.values;
    for (const field of ["contextWindow", "maxContextWindow", "maxTokens"] as const) {
      if (values[field] !== undefined && (!Number.isSafeInteger(values[field]) || values[field]! <= 0)) throw new Error(`Invalid catalog ${field}`);
    }
    if (values.contextWindow !== undefined && values.maxContextWindow !== undefined && values.maxContextWindow < values.contextWindow) throw new Error("Invalid catalog extended context window");
    if (values.reasoning !== undefined && typeof values.reasoning !== "boolean") throw new Error("Invalid catalog reasoning");
    if (values.thinkingConfig !== undefined && (!values.thinkingConfig || !sameModelParameter(parseModelThinkingConfig(values.thinkingConfig), values.thinkingConfig))) throw new Error("Unsupported catalog thinking configuration");
  }
  return doc;
}

export function createModelCatalogService(options: { cachePath: string; fetch: typeof globalThis.fetch; builtin?: ModelCatalogDocument; verifySignature?: CatalogSignatureVerifier }) {
  let current = options.builtin ?? BUILTIN_MODEL_CATALOG;
  let source: ModelCatalogStatus["source"] = "builtin";
  const revisions = new Map([[current.version, current]]);
  let loaded: Promise<void> | undefined;
  const status = (): ModelCatalogStatus => ({ version: current.version, upstreamCommit: current.upstreamCommit, source });
  async function load() {
    loaded ??= (async () => {
      try {
        // The cache stores the exact bytes that were signed, beside their
        // signature. Without that, anything able to write the agent directory
        // could plant a catalog the next launch trusts as `source: "updated"` —
        // and because `refresh` short-circuits when the version matches, a plant
        // at the current version would never be replaced by a genuine update.
        const envelope = JSON.parse(await readFile(options.cachePath, "utf8"));
        if (envelope?.schema !== 1 || typeof envelope.catalog !== "string" || typeof envelope.signature !== "string") throw new Error("Malformed catalog cache");
        verifyCatalogSignature(new TextEncoder().encode(envelope.catalog), envelope.signature, options.verifySignature);
        current = decodeModelCatalog(JSON.parse(envelope.catalog));
        source = "updated"; revisions.set(current.version, current);
      } catch { /* Offline, first launch, or a cache this build cannot authenticate. */ }
    })();
    await loaded;
  }
  return {
    async status() { await load(); return status(); },
    async resolve(input: ModelAdaptationInput) {
      await load();
      return resolveModelAdaptation(revisions.get(input.catalogVersion ?? current.version) ?? current, input);
    },
    async refresh(): Promise<ModelCatalogRefreshResult> {
      await load();
      const signal = AbortSignal.timeout(10_000);
      const [catalogBytes, signatureBytes] = await Promise.all([
        boundedBytes(await options.fetch(MODEL_CATALOG_URL, { signal }), MODEL_CATALOG_MAX_BYTES),
        boundedBytes(await options.fetch(MODEL_CATALOG_SIGNATURE_URL, { signal }), MODEL_CATALOG_MAX_BYTES),
      ]);
      // Verified before anything is decoded, cached or adopted: a rejected
      // catalog leaves `current`, `source` and the cache file untouched.
      verifyCatalogSignature(catalogBytes, new TextDecoder().decode(signatureBytes), options.verifySignature);
      const body = new TextDecoder().decode(catalogBytes);
      const next = decodeModelCatalog(JSON.parse(body));
      if (next.version === current.version) return { updated: false, catalog: status(), message: "模型目录已是最新版本" };
      await mkdir(dirname(options.cachePath), { recursive: true });
      const envelope = JSON.stringify({ schema: 1, catalog: body, signature: new TextDecoder().decode(signatureBytes) });
      await writeFile(`${options.cachePath}.tmp`, envelope, "utf8");
      await rename(`${options.cachePath}.tmp`, options.cachePath);
      current = next; source = "updated"; revisions.set(next.version, next);
      return { updated: true, catalog: status(), message: "模型目录已更新；已保存配置和当前草稿保持不变，可主动重新应用推荐" };
    },
  };
}

type OriginRecord = { values: ModelParameterValues; sources: ModelParameterSources };
type OriginFile = Record<string, Record<string, OriginRecord>>;

/** YAML owns values; the sidecar only remembers where an unchanged value came from. */
export function createModelOriginStore(agentDir: string) {
  const path = join(agentDir, "studio-model-origins.v1.json");
  async function read(): Promise<OriginFile> {
    try { return JSON.parse(await readFile(path, "utf8")) as OriginFile; }
    catch { return {}; }
  }
  async function reader() {
    const file = await read();
    return (provider: string, id: string, values: ModelParameterValues): ModelParameterSources => {
      const record = file[provider]?.[id];
      return Object.fromEntries(MODEL_PARAMETER_FIELDS.filter(field => values[field] !== undefined).map(field => [field,
        record?.sources[field] && sameModelParameter(record.values[field], values[field]) ? record.sources[field] : { source: "manual" },
      ]));
    };
  }
  return {
    reader,
    async sources(provider: string, id: string, values: ModelParameterValues): Promise<ModelParameterSources> {
      return (await reader())(provider, id, values);
    },
    async save(provider: string, records: Record<string, OriginRecord>): Promise<void> {
      const file = await read();
      file[provider] = records;
      await mkdir(agentDir, { recursive: true });
      await writeFile(path, JSON.stringify(file, null, 2), "utf8");
    },
  };
}

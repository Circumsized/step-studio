import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { test } from "node:test";
import { mkdtemp, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { MODEL_THINKING_MODES, modelParameterValues, thinkingForRoleModel, clampRoleThinking, type ModelCatalogDocument, type ModelThinkingConfig } from "@step-studio/client-contract";
import {
  createModelCatalogService, resolveModelAdaptation, decodeModelCatalog,
  MODEL_CATALOG_URL, MODEL_CATALOG_SIGNATURE_URL, MODEL_CATALOG_PUBLIC_KEYS,
  createCatalogSignatureVerifier, verifyCatalogSignature, MODEL_CATALOG_MAX_BYTES,
} from "../src/model-adaptation.js";
import { BUILTIN_MODEL_CATALOG } from "../src/model-catalog.generated.js";
import { createOmpModelsService, toYamlProvider } from "../src/omp-models-adapter.js";
import { parseModelsYml } from "../src/models-yml.js";

const thinking: ModelThinkingConfig = { mode: "anthropic-adaptive", efforts: ["low", "high", "max"], defaultLevel: "high", effortMap: { max: "maximum" }, supportsDisplay: true, requiresEffort: true };
const catalog: ModelCatalogDocument = { schemaVersion: 1, version: "v1", upstreamCommit: "test", entries: [
  { key: "official/a", provider: "official", id: "model-a-2026", name: "Model A 2026", api: "anthropic-messages", aliases: ["model-a-stable"], values: { contextWindow: 200000, maxTokens: 64000, reasoning: true, thinkingConfig: thinking } },
] };

test("the published directory can be loaded through the same boundary as a remote update", () => {
  assert.equal(decodeModelCatalog(BUILTIN_MODEL_CATALOG).version, BUILTIN_MODEL_CATALOG.version);
});

test("matches names and aliases, respects the API, and lets endpoint values win", () => {
  const result = resolveModelAdaptation(catalog, { providerId: "gateway", api: "anthropic-messages", modelId: "Model_A_2026", endpoint: { contextWindow: 100000 } });
  assert.equal(result.status, "matched");
  assert.equal(result.values.contextWindow, 100000);
  assert.equal(result.sources.contextWindow?.source, "endpoint");
  assert.deepEqual(result.values.thinkingConfig, thinking);
  assert.equal(result.sources.thinkingConfig?.catalogModel, "official/a");
  assert.equal(resolveModelAdaptation(catalog, { providerId: "gateway", api: "anthropic-messages", modelId: "model-a-stable" }).status, "matched");
  assert.equal(resolveModelAdaptation(catalog, { providerId: "gateway", api: "openai-completions", modelId: "model-a-2026" }).status, "none");
});

test("different provider configurations remain ambiguous; exact providers take priority", () => {
  const conflicting: ModelCatalogDocument = { ...catalog, entries: [...catalog.entries, { ...catalog.entries[0]!, key: "other/a", provider: "other", values: { contextWindow: 64000 } }] };
  const input = { providerId: "gateway", api: "anthropic-messages", modelId: "model-a-2026" };
  const result = resolveModelAdaptation(conflicting, input);
  assert.equal(result.status, "ambiguous");
  assert.deepEqual(result.values, {});
  assert.equal(resolveModelAdaptation(conflicting, { ...input, providerId: "official" }).values.contextWindow, 200000);
  const identical = { ...catalog, entries: [...catalog.entries, { ...catalog.entries[0]!, provider: "other", key: "other/a" }] };
  assert.equal(resolveModelAdaptation(identical, input).status, "matched");
});

test("manual catalog refresh is offline at startup, preserves pinned drafts, and retains the cache after failure", async () => {
  const directory = await mkdtemp(join(tmpdir(), "omp-catalog-"));
  const next: ModelCatalogDocument = { ...catalog, version: "v2", entries: [{ ...catalog.entries[0]!, values: { contextWindow: 999999 } }] };
  let calls = 0;
  const fetch: typeof globalThis.fetch = async (url) => {
    calls += 1;
    if (calls > 2) throw new Error("offline");
    assert.ok(url === MODEL_CATALOG_URL || url === MODEL_CATALOG_SIGNATURE_URL, `unexpected url ${String(url)}`);
    return url === MODEL_CATALOG_SIGNATURE_URL
      ? Response.json({ schema: 1, keyId: "test", algorithm: "ed25519", value: "stub" })
      : Response.json(next);
  };
  const cachePath = join(directory, "cache.json");
  const service = createModelCatalogService({ cachePath, fetch, builtin: catalog, verifySignature: () => true });
  assert.equal((await service.status()).version, "v1"); assert.equal(calls, 0);
  assert.equal((await service.refresh()).catalog.version, "v2");
  const input = { providerId: "official", api: "anthropic-messages", modelId: "model-a-2026" };
  assert.equal((await service.resolve({ ...input, catalogVersion: "v1" })).values.contextWindow, 200000);
  assert.equal((await service.resolve(input)).values.contextWindow, 999999);
  await assert.rejects(service.refresh(), /offline/);
  assert.equal((await service.status()).version, "v2");
  // The cache is authenticated on load too, so this reader needs the same stub
  // verifier the writer used.
  assert.equal((await createModelCatalogService({ cachePath, fetch, builtin: catalog, verifySignature: () => true }).status()).version, "v2");
});

test("a catalog update must carry a signature a trusted key produced", () => {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const verifier = createCatalogSignatureVerifier({ "test-key": publicKey.export({ type: "spki", format: "pem" }).toString() });
  const payload = new TextEncoder().encode('{"schemaVersion":1,"version":"v1"}');
  const value = sign(null, payload, privateKey).toString("base64url");
  const good = { algorithm: "ed25519", keyId: "test-key", value };

  assert.equal(verifier(good, payload), true);
  // 字节被换、keyId 不在表里、算法不是 ed25519——三种都必拒。
  assert.equal(verifier(good, new TextEncoder().encode('{"schemaVersion":1,"version":"v2"}')), false);
  assert.equal(verifier({ ...good, keyId: "step-studio-release-2026a" }, payload), false);
  assert.equal(verifier({ ...good, algorithm: "rsa-sha256" }, payload), false);
  assert.equal(verifier({ ...good, value: `${value.slice(0, -2)}xy` }, payload), false);

  // 表里两把都是真公钥，且都不是上面那把测试钥匙的。
  for (const [keyId, pem] of Object.entries(MODEL_CATALOG_PUBLIC_KEYS)) {
    assert.match(pem, /^-----BEGIN PUBLIC KEY-----/u, keyId);
    assert.equal(createCatalogSignatureVerifier({ [keyId]: pem })(good, payload), false, `${keyId} must not accept a foreign signature`);
  }
});

test("verifyCatalogSignature refuses a malformed or unverifiable envelope", () => {
  const body = "{}";
  for (const bad of ["", "not json", "[]", "null", '{"algorithm":"ed25519"}', '{"keyId":"k","value":"v"}']) {
    assert.throws(() => verifyCatalogSignature(new TextEncoder().encode(body), bad, () => true), `envelope ${bad}`);
  }
  assert.throws(() => verifyCatalogSignature(new TextEncoder().encode(body), '{"algorithm":"ed25519","keyId":"k","value":"v"}', () => false), /does not verify/);
  assert.doesNotThrow(() => verifyCatalogSignature(new TextEncoder().encode(body), '{"algorithm":"ed25519","keyId":"k","value":"v"}', () => true));
});

test("refresh refuses a catalog whose signature does not verify, and never caches it", async () => {
  const directory = await mkdtemp(join(tmpdir(), "omp-catalog-"));
  const next: ModelCatalogDocument = { ...catalog, version: "v2", entries: [{ ...catalog.entries[0]!, values: { contextWindow: 999999 } }] };
  const fetch: typeof globalThis.fetch = async (url) =>
    url === MODEL_CATALOG_SIGNATURE_URL
      ? Response.json({ schema: 1, keyId: "step-studio-release-2026a", algorithm: "ed25519", value: "not-a-real-signature" })
      : Response.json(next);
  const cachePath = join(directory, "cache.json");
  const service = createModelCatalogService({ cachePath, fetch, builtin: catalog });
  await assert.rejects(service.refresh(), /does not verify/);
  assert.equal((await service.status()).version, "v1", "the poisoned catalog must not reach the cache or the live view");
  await assert.rejects(readFile(cachePath, "utf8"), /ENOENT/);
});

test("a cached catalog is re-authenticated on the next launch", async () => {
  const directory = await mkdtemp(join(tmpdir(), "omp-catalog-"));
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const pem = publicKey.export({ type: "spki", format: "pem" }).toString();
  const verifier = createCatalogSignatureVerifier({ "test-key": pem });
  const cachePath = join(directory, "cache.json");
  const body = JSON.stringify({ ...catalog, version: "v2", entries: [{ ...catalog.entries[0]!, values: { contextWindow: 999999 } }] });
  const signature = JSON.stringify({ schema: 1, keyId: "test-key", algorithm: "ed25519", value: sign(null, Buffer.from(body), privateKey).toString("base64url") });

  // A cache file this build cannot authenticate is ignored, not trusted: the
  // service falls back to the builtin rather than adopting planted bytes.
  await writeFile(cachePath, JSON.stringify({ schema: 1, catalog: body, signature: "not-it" }), "utf8");
  assert.equal((await createModelCatalogService({ cachePath, fetch: async () => { throw new Error("offline"); }, builtin: catalog, verifySignature: verifier }).status()).version, "v1");

  await writeFile(cachePath, JSON.stringify({ schema: 1, catalog: body, signature }), "utf8");
  assert.equal((await createModelCatalogService({ cachePath, fetch: async () => { throw new Error("offline"); }, builtin: catalog, verifySignature: verifier }).status()).version, "v2");
});

test("the signed bytes are the parsed bytes, BOM and all", () => {
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const verifier = createCatalogSignatureVerifier({ "test-key": publicKey.export({ type: "spki", format: "pem" }).toString() });
  // A BOM is the case where decoding then re-encoding is not the identity, so a
  // verifier that round-tripped through a string would reject a valid signature.
  const bytes = Buffer.concat([Buffer.from("\ufeff", "utf8"), Buffer.from('{"schemaVersion":1}')]);
  const value = sign(null, bytes, privateKey).toString("base64url");
  assert.doesNotThrow(() => verifyCatalogSignature(new Uint8Array(bytes), JSON.stringify({ keyId: "test-key", algorithm: "ed25519", value }), verifier));
  assert.throws(() => verifyCatalogSignature(new Uint8Array(Buffer.from('{"schemaVersion":1}')), JSON.stringify({ keyId: "test-key", algorithm: "ed25519", value }), verifier), /does not verify/);
});

test("refresh refuses a catalog larger than the bound instead of buffering it", async () => {
  const directory = await mkdtemp(join(tmpdir(), "omp-catalog-"));
  const oversized = "x".repeat(MODEL_CATALOG_MAX_BYTES + 1);
  const fetch: typeof globalThis.fetch = async (url) =>
    url === MODEL_CATALOG_SIGNATURE_URL
      ? new Response(new Blob([JSON.stringify({ schema: 1, keyId: "k", algorithm: "ed25519", value: "v" })]), { headers: { "content-length": "60" } })
      : new Response(new Blob([oversized]), { headers: { "content-length": String(oversized.length) } });
  const service = createModelCatalogService({ cachePath: join(directory, "cache.json"), fetch, builtin: catalog });
  await assert.rejects(service.refresh(), /too large|exceeds/);
});

test("complete thinking modes survive save/read, source records survive reopening, and external edits become manual", async () => {
  const directory = await mkdtemp(join(tmpdir(), "omp-model-origins-"));
  const service = createOmpModelsService({ agentDir: directory, locateOmp: async () => undefined });
  const models = MODEL_THINKING_MODES.map((mode, i) => ({ id: `model-${i}`, contextWindow: 200000, maxContextWindow: 1000000, maxTokens: 64000, reasoning: true, thinkingConfig: { ...thinking, mode }, parameterSources: { contextWindow: { source: "endpoint" as const }, thinkingConfig: { source: "catalog" as const, catalogVersion: "v1", catalogModel: "official/a" } } }));
  await service.upsertProvider({ id: "gateway", name: "Gateway", api: "anthropic-messages", auth: { type: "none" }, models });
  let snapshot = await service.get();
  const rows = snapshot.providers.find(row => row.id === "gateway")!.models;
  for (const [i, row] of rows.entries()) {
    assert.deepEqual(row.thinkingConfig, models[i]!.thinkingConfig);
    assert.equal(row.maxContextWindow, 1000000);
    assert.equal(row.parameterSources?.thinkingConfig?.source, "catalog");
    assert.equal(snapshot.availableModels.find(model => model.selector === row.selector)?.thinkingConfig?.defaultLevel, "high");
  }
  const file = join(directory, "models.yml");
  await writeFile(file, (await readFile(file, "utf8")).replace("contextWindow: 200000", "contextWindow: 100000"));
  snapshot = await createOmpModelsService({ agentDir: directory, locateOmp: async () => undefined }).get();
  const edited = snapshot.providers.find(row => row.id === "gateway")!.models[0]!;
  assert.equal(edited.parameterSources?.contextWindow?.source, "manual");
  assert.equal(edited.parameterSources?.thinkingConfig?.source, "catalog");
});

test("unknown reasoning stays absent and thinking undefined preserves while null clears", () => {
  const input = { id: "gateway", name: "Gateway", api: "openai-completions", auth: { type: "none" as const }, models: [{ id: "unknown" }] };
  const output = toYamlProvider(input, { models: [{ id: "unknown", thinking: { ...thinking, efforts: [...thinking.efforts], effortMap: { max: "maximum" } }, customNativeSetting: "kept" }] });
  const model = (output.models as Array<Record<string, unknown>>)[0]!;
  assert.equal(Object.hasOwn(model, "reasoning"), false);
  assert.deepEqual(model.thinking, thinking);
  assert.equal(model.customNativeSetting, "kept");
  const cleared = toYamlProvider({ ...input, models: [{ id: "unknown", thinkingConfig: null }] }, output);
  assert.equal(Object.hasOwn((cleared.models as Array<Record<string, unknown>>)[0]!, "thinking"), false);
});

test("the provider YAML overlay saves original sources and clears owned fields while retaining native extras", async () => {
  const directory = await mkdtemp(join(tmpdir(), "omp-overlay-origins-"));
  const service = createOmpModelsService({ agentDir: directory, locateOmp: async () => undefined });
  await service.writeModelsYml({ text: "", overlay: { id: "gateway", name: "Gateway", api: "openai-completions", auth: { type: "none" }, models: [{ id: "example", contextWindow: 200000, thinkingConfig: { ...thinking, mode: "effort" }, parameterSources: { contextWindow: { source: "catalog", catalogVersion: "v1" } } }] } });
  const model = (await service.get()).providers[0]!.models[0]!;
  assert.equal(model.parameterSources?.contextWindow?.source, "catalog");
  const rewritten = toYamlProvider({ id: "gateway", name: "Gateway", api: "openai-completions", auth: { type: "none" }, models: [{ id: "example", omitMaxOutputTokens: false }] }, { models: [{ id: "example", omitMaxOutputTokens: true, contextPromotionTarget: "old/model", nativeExtra: true }] });
  const row = (rewritten.models as Array<Record<string, unknown>>)[0]!;
  assert.equal(row.omitMaxOutputTokens, undefined);
  assert.equal(row.contextPromotionTarget, undefined);
  assert.equal(row.nativeExtra, true);
});

test("role defaults, nearest efforts with higher ties, explicit off and required effort", async () => {
  const model = { reasoning: true, thinkingConfig: { mode: "effort" as const, efforts: ["low", "high"] as const, defaultLevel: "high" as const } };
  assert.equal(thinkingForRoleModel(undefined, model), "high");
  assert.equal(clampRoleThinking("medium", model), "high");
  assert.equal(clampRoleThinking("off", model), "off");
  assert.equal(clampRoleThinking("off", { ...model, thinkingConfig: { ...model.thinkingConfig, requiresEffort: true } }), "low");
  const directory = await mkdtemp(join(tmpdir(), "omp-role-off-"));
  await writeFile(join(directory, "config.yml"), "modelRoles:\n  default: gateway/example:off\n  plan: gateway/example:medium\n  task: gateway/example\n");
  const service = createOmpModelsService({ agentDir: directory, locateOmp: async () => undefined });
  const snapshot = await service.get();
  assert.equal(snapshot.roles.find(role => role.id === "default")?.thinking, "off");
  assert.match(snapshot.generatedConfigYml, /gateway\/example:off/);
  assert.equal(snapshot.roles.find(role => role.id === "task")?.thinking, undefined);
});

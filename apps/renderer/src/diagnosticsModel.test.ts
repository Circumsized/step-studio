import { describe, expect, it } from "vitest";
import type { EnvironmentReadModel, RuntimeConnection } from "@step-studio/client-contract";
import {
  deriveDiagnosticsView,
  formatRuntimeAutoRespawnCopy,
  formatRuntimeClassification,
  formatRuntimeDisconnectCode,
  formatRuntimeStatusLabel,
  formatRuntimeUnavailableCopy,
  type I18nT,
} from "./diagnosticsModel";
import { translate } from "./i18n/I18nContext";
import type { Locale, TranslationParams } from "./i18n/types";

const COMMAND_MANIFEST_DRIFT = "managed runtime command manifest hash drift";

function localized(locale: Locale): I18nT {
  return (key, params) => translate(locale, key, params as TranslationParams | undefined);
}

function driftedRuntime(): RuntimeConnection {
  return {
    status: "unavailable",
    classification: "unavailable",
    unavailableCode: "resolution-rejected",
    unavailableReason: COMMAND_MANIFEST_DRIFT,
  } as RuntimeConnection;
}

function installedEnvironment(): EnvironmentReadModel {
  return {
    platform: "win32",
    arch: "x64",
    authority: {
      authorityId: "auth-1" as EnvironmentReadModel["authority"]["authorityId"],
      authorityEpoch: 1 as EnvironmentReadModel["authority"]["authorityEpoch"],
    },
    runtime: driftedRuntime(),
    installer: { status: "installed", version: "18.0.11-studio.15", signature: "unknown" },
  };
}

describe("diagnostics hero copy", () => {
  // The hero line reported `签名：{sig}` in the field: the template placeholder is
  // `sig` while the caller passed `signature`, and formatTranslation leaves an
  // unmatched placeholder verbatim. Assert on the rendered text, not the params.
  it("substitutes the signature placeholder in every locale", () => {
    for (const locale of ["zh", "en"] as const) {
      const view = deriveDiagnosticsView(
        {
          runtime: driftedRuntime(),
          environment: installedEnvironment(),
          diagnostics: { entries: [], generatedAt: "2026-09-04T13:22:17.000Z" } as never,
        },
        localized(locale),
      );
      expect(view.hero.detail).not.toMatch(/[{}]/u);
      expect(view.hero.detail).toContain(translate(locale, "diagnostics.signatureUnknown"));
    }
  });

  it("localizes a known resolver rejection instead of printing the host sentence", () => {
    for (const locale of ["zh", "en"] as const) {
      const copy = formatRuntimeUnavailableCopy("resolution-rejected", COMMAND_MANIFEST_DRIFT, localized(locale));
      expect(copy.detail).not.toContain(COMMAND_MANIFEST_DRIFT);
      expect(copy.problem).not.toContain(COMMAND_MANIFEST_DRIFT);
      expect(copy.detail).toBe(translate(locale, "diagnostics.rejectedCommandManifestDriftDetail"));
    }
  });

  // Losing a translation is better than losing evidence, so anything unmapped
  // still reaches the operator verbatim.
  it("falls back to the raw reason for an unmapped rejection", () => {
    const copy = formatRuntimeUnavailableCopy("resolution-rejected", "some brand new resolver reason");
    expect(copy.detail).toBe("some brand new resolver reason");
  });

  // A union is a compile-time promise, not a runtime one. The label tables are
  // typed `Record<Union, string>`, which reads as total; a Host on a newer
  // protocol — or any frame this build does not know — indexes them with a key
  // that is not there, and the lookup yields `undefined`. That used to reach
  // `t`, whose `path.split(".")` threw, and `deriveDiagnosticsView` went down
  // with it: the whole diagnostics panel, because of one unknown status string.
  // No test caught it because all of them drove in-union values only.
  describe("a status the Host sends that this build has no label for", () => {
    const UNKNOWN = "reconnecting";

    function runtimeWith(status: string): RuntimeConnection {
      return { status, classification: "managed" } as unknown as RuntimeConnection;
    }

    it("shows the raw status instead of throwing", () => {
      expect(formatRuntimeStatusLabel(UNKNOWN as never)).toBe(UNKNOWN);
      expect(formatRuntimeClassification(UNKNOWN as never)).toBe(UNKNOWN);
      expect(formatRuntimeAutoRespawnCopy(UNKNOWN as never)).toBe(UNKNOWN);
      expect(formatRuntimeDisconnectCode(UNKNOWN as never)).toBe(UNKNOWN);
    });

    it("still localizes a known status", () => {
      // The fallback must not swallow the mapped path.
      expect(formatRuntimeStatusLabel("connected")).toBe(
        translate("zh", "diagnostics.statusConnected"),
      );
    });

    it("derives a whole view instead of throwing", () => {
      const view = deriveDiagnosticsView({
        runtime: runtimeWith(UNKNOWN),
        checks: [],
        entries: [],
      } as unknown as Parameters<typeof deriveDiagnosticsView>[0]);
      // The unknown value reaches the operator verbatim rather than blanking
      // the panel — same rule the unmapped-reason test above asserts.
      expect(view.hero.title).toContain(UNKNOWN);
      expect(view.hero.kind).not.toBe("ok");
      const runtime = view.checks.find((check) => check.id === "runtime");
      expect(runtime?.detail).toContain(UNKNOWN);
      expect(runtime?.tone).toBe("error");
    });

    // The installer's signature field rides the same unvalidated path from
    // bootstrap.environment, and it was the one table index the first fix missed.
    it("shows the raw signature instead of throwing", () => {
      const body = deriveDiagnosticsView({
        environment: {
          runtime: { status: "connected", classification: "managed" },
          installer: { status: "installed", version: "1", signature: "brand-new-scheme" },
        },
        checks: [],
        entries: [],
      } as unknown as Parameters<typeof deriveDiagnosticsView>[0]);
      expect(body.hero.detail).toContain("brand-new-scheme");
    });

    // Unknown installer status, and no runtime at all, both used to reach the
    // `ok` hero: absence of evidence read as health.
    it("does not report an unknown installer status as healthy", () => {
      const body = deriveDiagnosticsView({
        environment: {
          runtime: { status: "connected", classification: "managed" },
          installer: { status: "half-installed", version: "1", signature: "verified" },
        },
        checks: [],
        entries: [],
      } as unknown as Parameters<typeof deriveDiagnosticsView>[0]);
      expect(body.hero.kind).not.toBe("ok");
      expect(body.hero.detail).toContain("half-installed");
    });

    it("does not report a missing runtime as healthy", () => {
      const body = deriveDiagnosticsView({
        environment: { installer: { status: "installed", version: "1", signature: "verified" } },
        checks: [],
        entries: [],
      } as unknown as Parameters<typeof deriveDiagnosticsView>[0]);
      expect(body.hero.kind).not.toBe("ok");
    });
  });
});

/**
 * The acceptance criterion for the M5.6 refactor: a capability that is absent
 * must produce a degraded state, never a crash and never a component that
 * silently is not there.
 *
 * The bug this module exists to prevent is not hypothetical. The renderer used
 * to ask `globalThis.ompStudioChrome` for a method and branch on whether it came
 * back, so a missing member took the whole component with it — which is why
 * deleting Electron cascaded into 55,092 lines rather than swapping a facade.
 */

import { afterEach, describe, expect, it } from "vitest";

import { shellCapabilities, shellDegradedReason, shellSurface, type ChromeCapabilities } from "./index";

/** Every bit, so a new one cannot be added without a test noticing. */
const ALL_CAPABILITIES: ReadonlyArray<keyof ChromeCapabilities> = [
  "artifacts",
  "serviceDefinitions",
  "projectIntegration",
  "mediaUpload",
  "liveAudio",
  "microphonePermission",
  "browserObservation",
  "logs",
  "menu",
  "platform",
  "window",
];

const original = globalThis.ompStudioChrome;

afterEach(() => {
  if (original === undefined) {
    // The property is declared with `var`, so deleting is the only way back.
    delete (globalThis as { ompStudioChrome?: unknown }).ompStudioChrome;
  } else {
    globalThis.ompStudioChrome = original;
  }
});

/** A surface that has exactly the members named, and nothing else. */
function surfaceWith(...names: ReadonlyArray<string>): void {
  const stub: Record<string, unknown> = {};
  for (const name of names) {
    stub[name] = () => Promise.resolve();
  }
  (globalThis as { ompStudioChrome?: unknown }).ompStudioChrome = stub;
}

describe("shellCapabilities", () => {
  it("reports nothing when there is no shell", () => {
    delete (globalThis as { ompStudioChrome?: unknown }).ompStudioChrome;
    const bits = shellCapabilities();
    for (const capability of ALL_CAPABILITIES) {
      expect(bits[capability], `${capability} must be false with no shell`).toBe(false);
    }
    expect(shellSurface()).toBeUndefined();
  });

  it("reports a capability only when every member it needs is wired", () => {
    // Half of the media-upload group: the capability is the group, not any one
    // method. A half-wired group would otherwise look available and then fail on
    // the member that is missing.
    surfaceWith("beginMediaUpload", "appendMediaUpload");
    expect(shellCapabilities().mediaUpload).toBe(false);

    surfaceWith("beginMediaUpload", "appendMediaUpload", "finishMediaUpload", "abortMediaUpload");
    expect(shellCapabilities().mediaUpload).toBe(true);
  });

  it("groups the members the way the shell-side traits do", () => {
    // HostApi: artifacts, service definitions, project integration.
    surfaceWith(
      "chooseArtifactDirectory",
      "importArtifact",
      "exportArtifact",
      "listServiceDefinitions",
      "saveServiceDefinition",
      "removeServiceDefinition",
      "openProjectInEditor",
      "openProjectDirectory",
    );
    const bits = shellCapabilities();
    expect(bits.artifacts).toBe(true);
    expect(bits.serviceDefinitions).toBe(true);
    expect(bits.projectIntegration).toBe(true);
    // And nothing else lights up because an unrelated group is present.
    expect(bits.mediaUpload).toBe(false);
    expect(bits.browserObservation).toBe(false);
  });
});

describe("shellDegradedReason", () => {
  it("has a reason for every capability, so no bit can render a blank", () => {
    // A capability with no reason string is the old failure in a new coat: the
    // component would disappear and the user would have nothing to read.
    for (const capability of ALL_CAPABILITIES) {
      const reason = shellDegradedReason(capability);
      expect(reason.length, `${capability} needs a reason`).toBeGreaterThan(0);
    }
  });

  it("says what is missing rather than reporting an error", () => {
    // "The current shell does not support X" is a fact about the machine. "X
    // failed" is a claim about an attempt that was never made.
    expect(shellDegradedReason("browserObservation")).toContain("不支持");
    expect(shellDegradedReason("browserObservation")).not.toContain("失败");
  });
});

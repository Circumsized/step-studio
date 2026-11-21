/**
 * The acceptance criterion for the M5.6 refactor: a capability that is absent
 * must produce a degraded state, never a crash and never a component that
 * silently is not there.
 *
 * The bug this module exists to prevent is not hypothetical. The renderer used
 * to ask `globalThis.stepStudioChrome` for a method and branch on whether it came
 * back, so a missing member took the whole component with it — which is why
 * deleting Electron cascaded into 55,092 lines rather than swapping a facade.
 */

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { shellCapabilities, shellDegradedReason, shellSurface, type ChromeCapabilities } from "./index";

/**
 * The one structural rule of this module: outside `chrome/`, nothing reads
 * `stepStudioChrome` directly.
 *
 * It is asserted by reading the tree rather than by review because it has already
 * been broken twice in ways that looked fine. The first refactor counted 86 reads
 * of `globalThis.stepStudioChrome` and cleared them — and left four behind that
 * spelled it `window.stepStudioChrome`, which a grep for the `globalThis` form
 * reported as zero. A rule that is only checked by the search that found the last
 * violation is not checked.
 */
describe("nothing outside chrome/ reads the surface directly", () => {
  // Both rules walk the same tree, so the root is resolved once for the suite.
  const root = join(import.meta.dirname, "..");

  it("holds for every source file under src/", () => {
    const offenders: string[] = [];
    const walk = (directory: string) => {
      for (const entry of readdirSync(directory)) {
        const path = join(directory, entry);
        if (statSync(path).isDirectory()) {
          if (entry !== "chrome") walk(path);
          continue;
        }
        if (!/\.tsx?$/.test(entry) || entry.endsWith(".test.ts") || entry.endsWith(".test.tsx")) continue;
        // The global's own declaration and prose that names it are not reads.
        if (entry === "vite-env.d.ts") continue;
        const text = readFileSync(path, "utf8");
        // Comments and strings that merely mention the name are not reads either,
        // so only a member access or a property lookup counts.
        if (/\.stepStudioChrome\b|\(globalThis as[^)]*stepStudioChrome/.test(text)) {
          offenders.push(relative(root, path));
        }
      }
    };
    walk(root);
    expect(offenders).toEqual([]);
  });

  it("no file gates a capability on a member existing", () => {
    // The rule above catches *reading the global*. This one catches the thing the
    // facade actually exists to prevent: using `shellSurface()?.someMember` as a
    // boolean, which answers "is this available?" by asking whether one member
    // happens to be wired.
    //
    // It is a separate rule because the first version had only the first one, and
    // the bug class it missed is exactly the one that shipped: ServicesPane gated
    // its delete button on `busy` alone and then called
    // `shellSurface()?.removeServiceDefinition?.(...)`, ComputerPane derived its
    // capability from `!!shellSurface()?.readComputerCapture`, and both panes
    // hand-rolled five member checks each. All of them green under the rule above.
    //
    // Deliberately narrow: `shellSurface()?.x` inside a call expression is fine
    // (`await shellSurface()?.foo(args)`), so only the truthiness positions count
    // — `if`, `&&`, `||`, `!`, `?:`, and a bare assignment. A wider pattern would
    // fire on every legitimate call, and a guard that cries wolf gets deleted.
    const offenders: string[] = [];
    const walk = (directory: string) => {
      for (const entry of readdirSync(directory)) {
        const path = join(directory, entry);
        if (statSync(path).isDirectory()) {
          if (entry !== "chrome") walk(path);
          continue;
        }
        if (!/\.tsx?$/.test(entry) || entry.endsWith(".test.ts") || entry.endsWith(".test.tsx")) continue;
        const text = readFileSync(path, "utf8");
        const lines = text.split("\n");
        for (const [index, line] of lines.entries()) {
          // Strip comments so prose mentioning the pattern does not count.
          const code = line.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/.*$/, "");
          //
          // The first cut of this pattern also matched `const x = shellSurface()?.y;`
          // and `!shellSurface()?.z?.()`, which are legitimate — a local bound so
          // the call below can use `?.`. It reported 11 sites, 10 of them fine.
          // A guard that cries wolf is a guard people delete, so the pattern is
          // the narrow one: a truthiness test (`if`, `&&`, `||`, `?:`, `disabled=`)
          // or a `!!` coercion, never a bare binding.
          const hit =
            /(?:if|while)\s*\(\s*!*\s*shellSurface\(\)\?\.\w+/.test(code)
            || /\?\?\s*!*\s*shellSurface\(\)\?\.\w+/.test(code)
            || /&&\s*!*\s*shellSurface\(\)\?\.\w+/.test(code)
            || /\|\|\s*!*\s*shellSurface\(\)\?\.\w+/.test(code)
            || /!!\s*shellSurface\(\)\?\.\w+/.test(code)
            || /disabled=\{[^}]*shellSurface\(\)\?\.\w+/.test(code);
          if (hit) offenders.push(`${relative(root, path)}:${index + 1}: ${code.trim().slice(0, 120)}`);
        }
      }
    };
    walk(root);
    expect(offenders, `capability derived from a member:\n${offenders.join("\n")}`).toEqual([]);
  });
});

/** Every bit, so a new one cannot be added without a test noticing. */
const ALL_CAPABILITIES: ReadonlyArray<keyof ChromeCapabilities> = [
  "artifacts",
  "serviceDefinitions",
  "projectIntegration",
  "mediaUpload",
  "liveAudio",
  "microphonePermission",
  "browserObservation",
  "computerCapture",
  "skillshareToken",
  "runtimeUpgrade",
  "conversationView",
  "prediction",
  "predictionImport",
  "logs",
  "menu",
  "platform",
  "window",
];

const original = globalThis.stepStudioChrome;

afterEach(() => {
  if (original === undefined) {
    // The property is declared with `var`, so deleting is the only way back.
    delete (globalThis as { stepStudioChrome?: unknown }).stepStudioChrome;
  } else {
    globalThis.stepStudioChrome = original;
  }
});

/** A surface that has exactly the members named, and nothing else. */
function surfaceWith(...names: ReadonlyArray<string>): void {
  const stub: Record<string, unknown> = {};
  for (const name of names) {
    stub[name] = () => Promise.resolve();
  }
  (globalThis as { stepStudioChrome?: unknown }).stepStudioChrome = stub;
}

describe("shellCapabilities", () => {
  it("reports nothing when there is no shell", () => {
    delete (globalThis as { stepStudioChrome?: unknown }).stepStudioChrome;
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
    expect(bits.computerCapture).toBe(false);
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
    expect(shellDegradedReason("computerCapture")).toContain("不支持");
    expect(shellDegradedReason("computerCapture")).not.toContain("失败");
  });
});

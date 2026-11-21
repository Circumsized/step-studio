/**
 * The `stepStudioTerminal` bridge: fan-out semantics and command-name drift.
 *
 * The fan-out is the part worth pinning. Tauri's `listen` returns a promise, so
 * a per-subscription registration would leave a window where the returned
 * unsubscribe finds nothing to remove and the listener outlives the component.
 * Registering once and fanning out to a Set keeps `onData`/`onExit` synchronous
 * and identity-exact — the same reason the desktop sender does it.
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, test } from "node:test";

import { createTauriTerminalBridge } from "../src/terminal-bridge.js";

/** A `listen` that records the handler so a test can fire events into it. */
function fakeListen(): {
  listen: <T>(event: string, handler: (event: { event: string; payload: T }) => void) => Promise<() => void>;
  fire(channel: string, payload: unknown): void;
  registered: string[];
} {
  const handlers = new Map<string, (event: { event: string; payload: unknown }) => void>();
  const registered: string[] = [];
  return {
    registered,
    listen: async (event, handler) => {
      registered.push(event);
      handlers.set(event, handler as never);
      return () => handlers.delete(event);
    },
    fire: (channel, payload) => {
      const handler = handlers.get(channel);
      if (handler === undefined) throw new Error(`nothing listens on ${channel}`);
      handler({ event: channel, payload });
    },
  };
}

describe("the terminal bridge registers one listener per channel, up front", () => {
  test("two subscribers share one registration", () => {
    const fake = fakeListen();
    const bridge = createTauriTerminalBridge(fake.listen as never);
    const seen: string[] = [];
    const offFirst = bridge.onData(event => seen.push(`a:${event.data}`));
    const offSecond = bridge.onData(event => seen.push(`b:${event.data}`));

    // Both channels are registered once, at construction, before any
    // subscriber exists — otherwise an event between the first subscribe and the
    // registration resolving would be dropped.
    assert.deepEqual(fake.registered, ["step-studio:terminal:data", "step-studio:terminal:exit"]);
    fake.fire("step-studio:terminal:data", { id: "t1", data: "hi" });
    assert.deepEqual(seen, ["a:hi", "b:hi"], "both subscribers see the event");

    // Identity-exact removal: taking one back must not disturb the other.
    offFirst();
    seen.length = 0;
    fake.fire("step-studio:terminal:data", { id: "t1", data: "again" });
    assert.deepEqual(seen, ["b:again"], "only the removed listener stopped");
    offSecond();
  });

  test("the exit channel is separate", () => {
    const fake = fakeListen();
    const bridge = createTauriTerminalBridge(fake.listen as never);
    const exits: string[] = [];
    bridge.onExit(event => exits.push(event.id));
    assert.deepEqual(fake.registered, ["step-studio:terminal:data", "step-studio:terminal:exit"]);
    fake.fire("step-studio:terminal:exit", { id: "t9" });
    assert.deepEqual(exits, ["t9"]);
  });
});

describe("command names match the Rust side", () => {
  test("every command the bridge invokes is declared in Rust", () => {
    const packageRoot = join(import.meta.dirname, "..", "..");
    const bridge = readFileSync(join(packageRoot, "src", "terminal-bridge.ts"), "utf8");
    const rust = readFileSync(join(packageRoot, "src-tauri", "src", "terminal.rs"), "utf8");

    const invoked = new Set([...bridge.matchAll(/tauriInvoke<[^>]*>\(\s*"(\w+)"/g)].map(m => m[1] ?? ""));
    const declared = new Set([...rust.matchAll(/#\[tauri::command\]\s*\npub fn (\w+)/g)].map(m => m[1] ?? ""));

    assert.ok(invoked.size >= 4, "the bridge must invoke the four lifecycle commands");
    for (const name of invoked) {
      assert.ok(declared.has(name), `terminal-bridge.ts invokes "${name}" but terminal.rs declares no such command`);
    }
    for (const name of declared) {
      assert.ok(invoked.has(name), `terminal.rs declares "${name}" but terminal-bridge.ts never invokes it`);
    }
  });

  test("the recording trio stays absent, because it needs the Host", () => {
    const bridge = createTauriTerminalBridge(fakeListen().listen as never) as unknown as Record<string, unknown>;
    for (const absent of ["recordingStart", "recordingStop", "recordingStatus"]) {
      assert.equal(bridge[absent], undefined, `${absent} must not be exposed by this shell`);
    }
  });
});

describe("the channel names match what Rust emits", () => {
  test("the JS constants are byte-identical to the Rust ones", () => {
    const packageRoot = join(import.meta.dirname, "..", "..");
    const bridge = readFileSync(join(packageRoot, "src", "terminal-bridge.ts"), "utf8");
    const rust = readFileSync(join(packageRoot, "src-tauri", "src", "terminal.rs"), "utf8");
    for (const name of ["TERMINAL_DATA_CHANNEL", "TERMINAL_EXIT_CHANNEL"]) {
      const js = bridge.match(new RegExp(`${name} = "([^"]+)"`));
      const rs = rust.match(new RegExp(`pub const ${name}: &str = "([^"]+)"`));
      assert.ok(js && rs, `${name} must be declared on both sides`);
      assert.equal(js[1], rs[1], `${name} differs between JS and Rust`);
    }
  });
});

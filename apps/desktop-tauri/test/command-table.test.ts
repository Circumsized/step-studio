/**
 * Completeness and anti-drift tests for the Tauri command table.
 *
 * The table in `src/tauri-sender.ts` is the single routing source between the
 * fixed `DESKTOP_IPC_CHANNELS` constants and the `#[tauri::command]` names in
 * `src-tauri/src/bridge.rs`. Two ways it can rot: a channel constant gains a
 * sibling with no row, or the Rust side renames a command. Both are asserted
 * here — the second by reading the Rust sources, which is the only guard that
 * spans the language boundary.
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, test } from "node:test";

import { DESKTOP_IPC_CHANNELS } from "@step-studio/transport-desktop";

import { DESKTOP_COMMAND_PREFIX } from "../src/tauri-sender.js";
import { createTauriDesktopSender, type TauriInvokeLike } from "../src/tauri-sender.js";

function recordingSender(): { invoke: TauriInvokeLike; commands: string[] } {
  const commands: string[] = [];
  const invoke: TauriInvokeLike = async <T>(command: string) => {
    commands.push(command);
    return undefined as T;
  };
  return { invoke, commands };
}

describe("the Tauri command table covers the Host bridge completely", () => {
  test("every invokable channel constant routes to a command named after it", async () => {
    const { invoke, commands } = recordingSender();
    const sender = createTauriDesktopSender(invoke, async () => () => {});
    const invokable = Object.entries(DESKTOP_IPC_CHANNELS).filter(
      ([, channel]) => channel !== DESKTOP_IPC_CHANNELS.event,
    );
    assert.ok(invokable.length > 0, "the contract must declare invokable channels");
    for (const [key, channel] of invokable) {
      commands.length = 0;
      await sender.invoke(channel);
      assert.deepEqual(commands, [channel.slice(DESKTOP_COMMAND_PREFIX.length)], `${key} has no command row`);
    }
  });

  test("the event channel is an event name and never a command", async () => {
    const { invoke, commands } = recordingSender();
    const sender = createTauriDesktopSender(invoke, async () => () => {});
    // It is the one channel you listen on, and the one you may never invoke.
    sender.on(DESKTOP_IPC_CHANNELS.event, () => {});
    await assert.rejects(() => sender.invoke(DESKTOP_IPC_CHANNELS.event), /unknown channel/);
    assert.deepEqual(commands, []);
  });

  test("the Rust side declares exactly the commands the table routes to", () => {
    // `rootDir` is "." so dist mirrors the source tree: the package root is
    // two levels up from dist/test, where src-tauri lives.
    const packageRoot = join(import.meta.dirname, "..", "..");
    const bridge = readFileSync(join(packageRoot, "src-tauri/src/bridge.rs"), "utf8");
    const main = readFileSync(join(packageRoot, "src-tauri/src/main.rs"), "utf8");
    const invokable = Object.values(DESKTOP_IPC_CHANNELS)
      .filter(channel => channel !== DESKTOP_IPC_CHANNELS.event)
      .map(channel => channel.slice(DESKTOP_COMMAND_PREFIX.length))
      .sort();
    for (const command of invokable) {
      assert.ok(bridge.includes(`pub fn ${command}(`), `bridge.rs is missing #[tauri::command] ${command}`);
      assert.ok(main.includes(`bridge::${command},`), `main.rs does not register bridge::${command}`);
    }
    const registered = [...main.matchAll(/bridge::(\w+),/g)].map(match => match[1]).sort();
    assert.deepEqual(registered, invokable, "main.rs registers a command the table does not route to");
  });
});

// The event channel is the one that carries everything, and it is the one
// channel `command-table` deliberately filters out of the invoke table — so it
// has no other guard. `terminal-bridge.test.ts` already pins the terminal
// channels byte-for-byte; this applies the same pattern to the event channel,
// where a rename on either side would silently stop every event rather than
// fail a command.
describe("the event channel matches what Rust emits", () => {
  test("the JS constant is byte-identical to the Rust one", () => {
    // Same convention as the tests above: `packageRoot` is the app package, so
    // the repo root is two up. It resolves differently when run from the source
    // than from `dist/test/`, which is what the gate runs — hence the shared
    // convention rather than a path from this file.
    const packageRoot = join(import.meta.dirname, "..", "..");
    const repoRoot = join(packageRoot, "..", "..");
    const ts = readFileSync(
      join(repoRoot, "packages", "transport-desktop", "src", "channels.ts"),
      "utf8",
    );
    const rust = readFileSync(join(packageRoot, "src-tauri", "src", "pump.rs"), "utf8");
    const js = ts.match(/event: "([^"]+)"/);
    const rs = rust.match(/pub const EVENT_CHANNEL: &str = "([^"]+)"/);
    assert.ok(js && rs, "the event channel must be declared on both sides");
    assert.equal(js[1], rs[1], "the event channel differs between JS and Rust");
  });
});

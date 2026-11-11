/**
 * Headless tests for the Tauri Desktop IPC sender.
 *
 * No Tauri runtime and no WebView: `invoke` and `listen` are injected, so
 * nothing here touches `window.__TAURI_INTERNALS__`. These tests defend the
 * three adaptations the sender makes for Tauri (command-name mapping, the
 * single shared listener, the rejection re-wrap) plus the one contract that
 * is easiest to get wrong when the chrome/terminal bridges arrive in M1.2:
 * the fan-out must deliver `(event, payload)` — two arguments — because
 * `subscribeChannel` destructures the second one.
 */

import assert from "node:assert/strict";
import { describe, test } from "node:test";

import type { ClientEvent, SubscriptionScope } from "@omp-studio/client-contract";
import {
  createDesktopIpcBridge,
  DESKTOP_IPC_CHANNELS,
  type DesktopIpcBridge,
} from "@omp-studio/transport-desktop";

import {
  createTauriDesktopSender,
  type TauriInvokeLike,
  type TauriListenLike,
} from "../src/tauri-sender.js";

interface RecordedInvoke {
  readonly command: string;
  readonly args: Record<string, unknown> | undefined;
}

interface FakeTauri {
  readonly sender: DesktopIpcBridge;
  readonly invokes: RecordedInvoke[];
  readonly listenCalls: string[];
  emit(payload: unknown): void;
  readonly stop: () => void;
}

function fakeTauri(): FakeTauri {
  const invokes: RecordedInvoke[] = [];
  const listenCalls: string[] = [];
  let handler: ((event: { event: string; payload: unknown }) => void) | undefined;
  let stopped = false;

  const invoke: TauriInvokeLike = async <T>(command: string, args?: Record<string, unknown>) => {
    invokes.push({ command, args });
    return undefined as T;
  };
  const listen: TauriListenLike = async <T>(
    event: string,
    received: (event: { event: string; payload: T }) => void,
  ) => {
    listenCalls.push(event);
    handler = received as (event: { event: string; payload: unknown }) => void;
    return () => {
      stopped = true;
    };
  };

  return {
    sender: createTauriDesktopSender(invoke, listen),
    invokes,
    listenCalls,
    emit(payload: unknown) {
      handler?.({ event: DESKTOP_IPC_CHANNELS.event, payload });
    },
    stop: () => {
      if (!stopped) throw new Error("unlisten was never called");
    },
  };
}

function event(kind: string): ClientEvent {
  return { kind } as unknown as ClientEvent;
}

describe("createTauriDesktopSender: positional arguments become named command arguments", () => {
  test("bootstrap and close invoke their commands with an empty argument object", async () => {
    const tauri = fakeTauri();
    await tauri.sender.invoke(DESKTOP_IPC_CHANNELS.bootstrap);
    await tauri.sender.invoke(DESKTOP_IPC_CHANNELS.close);
    assert.deepEqual(
      tauri.invokes.map(row => row.command),
      ["bootstrap", "close"],
    );
    assert.deepEqual(tauri.invokes[0]?.args, {});
    assert.deepEqual(tauri.invokes[1]?.args, {});
  });

  test("query and command pass the envelope under the command's argument name", async () => {
    const tauri = fakeTauri();
    const request = { name: "session.list", input: {} };
    await tauri.sender.invoke(DESKTOP_IPC_CHANNELS.query, request);
    await tauri.sender.invoke(DESKTOP_IPC_CHANNELS.command, request);
    assert.deepEqual(tauri.invokes[0], { command: "query", args: { request } });
    assert.deepEqual(tauri.invokes[1], { command: "command", args: { request } });
  });

  test("subscribe passes the scope under the subscribe command's argument name", async () => {
    const tauri = fakeTauri();
    const scope: SubscriptionScope = { scope: "all" };
    await tauri.sender.invoke(DESKTOP_IPC_CHANNELS.subscribe, scope);
    assert.deepEqual(tauri.invokes[0], { command: "subscribe", args: { scope } });
  });

  test("an argument the table has no name for is refused, never dropped", async () => {
    const tauri = fakeTauri();
    await assert.rejects(() => tauri.sender.invoke(DESKTOP_IPC_CHANNELS.bootstrap, "extra"), /arguments for 0 names/);
    assert.equal(tauri.invokes.length, 0);
  });

  test("a channel outside the fixed set is refused", async () => {
    const tauri = fakeTauri();
    await assert.rejects(() => tauri.sender.invoke("omp-studio:desktop:invented"), /unknown channel/);
    assert.equal(tauri.invokes.length, 0);
  });
});

describe("createTauriDesktopSender: one Tauri listener fans out to every subscription", () => {
  test("three subscriptions share a single Tauri event listener", () => {
    const tauri = fakeTauri();
    const api = createDesktopIpcBridge(tauri.sender);
    api.subscribe({ scope: "all" }, () => {});
    api.subscribe({ scope: "all" }, () => {});
    api.subscribe({ scope: "runtime" }, () => {});
    assert.deepEqual(tauri.listenCalls, [DESKTOP_IPC_CHANNELS.event]);
  });

  test("the fan-out delivers (event, payload) so the second argument carries the envelope", () => {
    const tauri = fakeTauri();
    const seen: unknown[][] = [];
    tauri.sender.on(DESKTOP_IPC_CHANNELS.event, (...args) => {
      seen.push(args);
    });
    const payload = event("conversation.message.delta");
    tauri.emit(payload);
    assert.equal(seen.length, 1);
    const args = seen[0];
    assert.equal(args?.length, 2, "subscribeChannel destructures the second argument");
    assert.equal(args?.[1], payload);
  });

  test("the real bridge receives scoped events through the same fan-out", () => {
    const tauri = fakeTauri();
    const api = createDesktopIpcBridge(tauri.sender);
    const received: ClientEvent[] = [];
    api.subscribe({ scope: "all" }, (received_) => {
      received.push(received_);
    });
    const payload = event("conversation.message.delta");
    tauri.emit(payload);
    assert.deepEqual(received, [payload]);
  });

  test("unsubscribe removes the exact listener, leaving siblings subscribed", () => {
    const tauri = fakeTauri();
    const api = createDesktopIpcBridge(tauri.sender);
    const first: ClientEvent[] = [];
    const second: ClientEvent[] = [];
    const offFirst = api.subscribe({ scope: "all" }, (received_) => {
      first.push(received_);
    });
    api.subscribe({ scope: "all" }, (received_) => {
      second.push(received_);
    });
    offFirst();
    const payload = event("conversation.message.delta");
    tauri.emit(payload);
    assert.deepEqual(first, []);
    assert.deepEqual(second, [payload]);
  });

  test("a second unsubscribe is a safe no-op", () => {
    const tauri = fakeTauri();
    const api = createDesktopIpcBridge(tauri.sender);
    const received: ClientEvent[] = [];
    const unsubscribe = api.subscribe({ scope: "all" }, (received_) => {
      received.push(received_);
    });
    unsubscribe();
    unsubscribe();
    tauri.emit(event("conversation.message.delta"));
    assert.deepEqual(received, []);
  });

  test("the event channel is never invokable and unknown event channels are refused", () => {
    const tauri = fakeTauri();
    assert.throws(() => tauri.sender.on("omp-studio:desktop:invented", () => {}), /unknown event channel/);
    assert.throws(() => tauri.sender.removeListener("omp-studio:desktop:invented", () => {}), /unknown event channel/);
    assert.deepEqual(tauri.listenCalls, [DESKTOP_IPC_CHANNELS.event]);
  });
});

describe("createTauriDesktopSender: rejections look like Electron's", () => {
  test("a structured Rust rejection surfaces as an Error carrying the host message", async () => {
    const invoke: TauriInvokeLike = async <T>() => {
      throw { code: "SOME_CODE", message: "host said no" };
    };
    const sender = createTauriDesktopSender(invoke, async () => () => {});
    await assert.rejects(() => sender.invoke(DESKTOP_IPC_CHANNELS.query, {}), error => {
      assert.ok(error instanceof Error);
      assert.equal(error.message, "host said no");
      return true;
    });
  });

  test("a string rejection keeps its message instead of becoming a generic failure", async () => {
    const invoke: TauriInvokeLike = async <T>() => {
      throw "host said no";
    };
    const sender = createTauriDesktopSender(invoke, async () => () => {});
    await assert.rejects(() => sender.invoke(DESKTOP_IPC_CHANNELS.bootstrap), /host said no/);
  });

  test("a rejection with no message still produces a usable Error", async () => {
    const invoke: TauriInvokeLike = async <T>() => {
      throw { code: "SOME_CODE" };
    };
    const sender = createTauriDesktopSender(invoke, async () => () => {});
    await assert.rejects(() => sender.invoke(DESKTOP_IPC_CHANNELS.bootstrap), /host rejected the request/);
  });

  test("a rejected listener registration does not break the unsubscribe", async () => {
    const listen: TauriListenLike = async () => {
      throw new Error("no internals");
    };
    const sender = createTauriDesktopSender(async <T>() => undefined as T, listen);
    const api = createDesktopIpcBridge(sender);
    const received: ClientEvent[] = [];
    const unsubscribe = api.subscribe({ scope: "all" }, (received_) => {
      received.push(received_);
    });
    unsubscribe();
    assert.deepEqual(received, []);
  });
});

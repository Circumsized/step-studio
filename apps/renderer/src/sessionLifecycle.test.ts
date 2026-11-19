import { describe, expect, it, vi } from "vitest";
import { MemoryClientTransport, StudioClientImpl } from "@omp-studio/client";
import type { ClientBootstrap } from "@omp-studio/client-contract";
import type { SessionId, StudioClient, ThreadId } from "@omp-studio/client-contract";
import { ensureSelectedSessionActive, waitForCommandReceipt } from "./sessionLifecycle";

const TS = "2026-08-12T00:00:00.000Z";

/**
 * The smallest snapshot a `StudioClientImpl` will accept. Only the fields the
 * reducer reads on `bootstrap.set` are load-bearing here; the rest exist to
 * satisfy the contract version check and the read-model shape.
 */
function bootstrapped(): ClientBootstrap {
  return {
    contractVersion: 2,
    authority: { authorityId: "auth-1" as never, authorityEpoch: 1 as never },
    runtime: {
      status: "connected",
      classification: "managed",
      runtimeId: "rt-1" as never,
      runtimeEpoch: 1 as never,
      backend: "studio-host",
      runtimeVersion: "1.0.0",
    },
    surface: { terminalAttach: false, fileReveal: false, previewInput: false, openExternal: false },
    capabilityManifest: { profile: "full-parity-v1", generatedAt: TS, hash: "cap-hash", capabilities: [] },
    commandManifestHash: "cmd-hash",
    selected: {},
    snapshot: {
      runtimeId: "rt-1" as never,
      runtimeEpoch: 1 as never,
      stateVersion: 1 as never,
      sessionId: "sess-1" as SessionId,
      isStreaming: false,
      isCompacting: false,
      activeMode: "normal",
      approvalMode: "yolo",
      pendingMessages: 0,
      activeCommandIds: [],
      agentsRevision: 0,
      jobsRevision: 0,
      agents: [],
      jobs: [],
    },
    stateVersion: 1 as never,
    cursor: "10" as never,
    residents: { generatedAt: TS, activeSessionId: "sess-1" as SessionId, residents: [] },
  };
}

function completedResumeClient(sessionId: SessionId) {
  const command = vi.fn(async () => ({ requestId: "resume-request" }));
  const client = {
    command,
    subscribe: () => () => {},
    getState: () => ({
      commands: {
        "resume-request": {
          requestId: "resume-request",
          name: "session.resume",
          status: "completed",
          result: { sessionId },
        },
      },
    }),
  } as unknown as StudioClient;
  return { client, command };
}

describe("ensureSelectedSessionActive", () => {
  it("resumes a loaded historical conversation before a write action", async () => {
    const selectedSessionId = "historical-session" as SessionId;
    const selectedThreadId = "historical-thread" as ThreadId;
    const { client, command } = completedResumeClient(selectedSessionId);

    await ensureSelectedSessionActive(client, {
      activeSessionId: "other-session" as SessionId,
      selectedSessionId,
      selectedThreadId,
    });

    expect(command).toHaveBeenCalledWith("session.resume", { threadId: selectedThreadId });
  });

  it("does not resume when the viewed session is already active", async () => {
    const selectedSessionId = "active-session" as SessionId;
    const { client, command } = completedResumeClient(selectedSessionId);

    await ensureSelectedSessionActive(client, {
      activeSessionId: selectedSessionId,
      selectedSessionId,
      selectedThreadId: "active-thread" as ThreadId,
    });

    expect(command).not.toHaveBeenCalled();
  });
});

/**
 * A client whose command is in flight, and whose reducer state can be moved
 * after the wait has already subscribed.
 */
function inFlightClient() {
  let status = "accepted";
  const watchers: Array<() => void> = [];
  const client = {
    subscribe: () => () => {},
    getState: () => ({
      commands: { "req-1": { requestId: "req-1", commandName: "runtime.ensure", status } },
    }),
    onState: (listener: () => void) => {
      watchers.push(listener);
      return () => {};
    },
  };
  return { client, moveTo: (next: string) => { status = next; for (const w of watchers) w(); } };
}

/** Never emits anything: no receipt event, no state change, no timeout escape. */
function silentClient() {
  return { subscribe: () => () => {}, getState: () => ({ commands: {} }) };
}

describe("waitForCommandReceipt", () => {
  // The reducer can move an in-flight command to a terminal status with no
  // matching `command.receipt` event — re-bootstrap, resync, a runtime epoch
  // change, runtime loss. Reading the command state once, before subscribing,
  // misses all four, and the wait then never settled at all: the caller waited
  // forever with no error and no way back. Live at DiagnosticsPage's
  // install/recheck, at runtimeEnsure, and at HomePage.
  it("settles when the reducer marks the command terminal after subscribing", async () => {
    const { client, moveTo } = inFlightClient();
    const waiting = waitForCommandReceipt(client as never, "req-1" as never);
    moveTo("outcome_unknown");
    const receipt = await Promise.race([
      waiting,
      new Promise((_, reject) => setTimeout(() => reject(new Error("the wait never settled")), 500)),
    ]);
    expect((receipt as { status: string }).status).toBe("outcome_unknown");
  });

  it("still settles on a real receipt event", async () => {
    let emit: ((event: unknown) => void) | undefined;
    const client = {
      subscribe: (_scope: unknown, listener: (event: unknown) => void) => {
        emit = listener;
        return () => {};
      },
      getState: () => ({ commands: {} }),
      onState: () => () => {},
    };
    const waiting = waitForCommandReceipt(client as never, "req-1" as never);
    emit?.({
      kind: "command.receipt",
      receipt: { requestId: "req-1", commandName: "runtime.ensure", status: "completed", result: {}, observedAt: "t" },
    });
    const receipt = await Promise.race([
      waiting,
      new Promise((_, reject) => setTimeout(() => reject(new Error("the wait never settled")), 500)),
    ]);
    expect((receipt as { status: string }).status).toBe("completed");
  });

  // Resolving with `outcome_unknown` rather than rejecting, because every caller
  // inspects `receipt.status` itself and already handles that status.
  it("gives up after its budget instead of waiting forever", async () => {
    const receipt = await waitForCommandReceipt(silentClient() as never, "req-1" as never, 60);
    expect(receipt.status).toBe("outcome_unknown");
    // `reason` lives on the outcome_unknown arm, so it needs narrowing.
    if (receipt.status === "outcome_unknown") expect(receipt.reason).toContain("超时");
  });

  // The budget has to hold when the caller passes none, which is every call site
  // today. Pinning it with fake timers so the test does not actually wait.
  it("bounds the wait by default, not only when a caller passes a budget", async () => {
    vi.useFakeTimers();
    try {
      const status = waitForCommandReceipt(silentClient() as never, "req-1" as never).then(
        (receipt) => receipt.status,
      );
      await vi.advanceTimersByTimeAsync(120_001);
      expect(await status).toBe("outcome_unknown");
    } finally {
      vi.useRealTimers();
    }
  });
});

/**
 * The four above hand-roll a client (`as never`), so each one encodes its own
 * guess at what `getState().commands[id]` looks like. That guess is the wiring:
 * if the real reducer nests the command state differently, or names the status
 * field differently, all four pass and every live call site still hangs.
 *
 * This one drives a real `StudioClientImpl` over a `MemoryClientTransport` —
 * the same pair the shipped renderer uses — so the state shape is the reducer's
 * own rather than a test's.
 */
describe("waitForCommandReceipt wired to a real StudioClientImpl", () => {
  it("settles when a re-bootstrap strands the command, with no receipt event", async () => {
    const transport = new MemoryClientTransport({
      bootstrap: () => bootstrapped(),
      command: (request) => ({ ...request, status: "accepted", acceptedAt: TS }),
    });
    const client = new StudioClientImpl(transport);
    await client.bootstrap();

    const handle = await client.command("session.resume", { threadId: "thread-1" as ThreadId });
    const waiting = waitForCommandReceipt(client, handle.requestId);

    // One of the four reducer paths that strand an in-flight command: a
    // re-bootstrap marks it `outcome_unknown` and emits no `command.receipt`.
    // Before the `onState` listener existed, this exact sequence hung the
    // caller forever — no error, no way back — at DiagnosticsPage's
    // install/recheck, at runtimeEnsure, and at HomePage.
    await client.bootstrap();

    const receipt = await Promise.race([
      waiting,
      new Promise((_, reject) => setTimeout(() => reject(new Error("the wait never settled")), 1000)),
    ]);
    expect((receipt as { status: string }).status).toBe("outcome_unknown");
    await client.close();
  });
});

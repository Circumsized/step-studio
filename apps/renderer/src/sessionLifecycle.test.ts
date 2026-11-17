import { describe, expect, it, vi } from "vitest";
import type { SessionId, StudioClient, ThreadId } from "@omp-studio/client-contract";
import { ensureSelectedSessionActive, waitForCommandReceipt } from "./sessionLifecycle";

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

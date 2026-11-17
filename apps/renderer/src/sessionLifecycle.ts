import type {
  ClientInteraction,
  CommandReceipt,
  CommandRequestId,
  CommandState,
  SessionHistoryEntry,
  SessionId,
  StudioClient,
  ThreadId,
} from "@omp-studio/client-contract";
import type { OperatorStateSnapshot } from "@omp-studio/studio-protocol";
import { waitReceipt } from "./hostError";

export const NEW_CONVERSATION_UNAVAILABLE_REASON =
  "Host 未提供 session.create，无法创建新的 Runtime 会话。";

export type NewSessionWaitResult =
  | { readonly ok: true; readonly sessionId?: SessionId }
  | { readonly ok: false; readonly error: string };

export function isNewConversationAvailable(): true {
  return true;
}

/** The single Host pending interaction, if exactly one command is waiting. */
export function selectPendingInteraction(
  commands: Readonly<Record<string, CommandState>>,
): ClientInteraction | null {
  const pending: ClientInteraction[] = [];
  for (const command of Object.values(commands)) {
    if (command.status === "interaction_required") {
      pending.push(command.interaction);
    }
  }
  if (pending.length === 0) return null;
  return pending[pending.length - 1] ?? null;
}

export type ResumeGenerationGate = {
  next(): number;
  isCurrent(generation: number): boolean;
};

export function createResumeGenerationGate(): ResumeGenerationGate {
  let current = 0;
  return {
    next() {
      current += 1;
      return current;
    },
    isCurrent(generation: number) {
      return generation === current;
    },
  };
}

export type CommandWaitClient = {
  subscribe(
    scope: { scope: "command"; requestId: CommandRequestId },
    listener: (event: { kind: string; receipt?: CommandReceipt }) => void,
  ): () => void;
  getState?: () => { readonly commands?: Readonly<Record<string, CommandState>> };
  /** Called on every reducer update. See `waitForCommandReceipt` for why it matters. */
  onState?: (listener: () => void) => () => void;
};

/**
 * How long a receipt may take before the wait gives up and reports
 * `outcome_unknown`. Same budget `waitReceipt` uses.
 */
const RECEIPT_TIMEOUT_MS = 120_000;

/**
 * The receipt a timed-out wait resolves with.
 *
 * Resolving rather than rejecting because every caller inspects
 * `receipt.status` itself — `outcome_unknown` is a status they already handle,
 * and the alternative (a rejected promise) would bypass that handling.
 */
function unknownReceipt(requestId: CommandRequestId): CommandReceipt {
  return {
    requestId,
    commandName: "runtime.ensure",
    status: "outcome_unknown",
    reason: "等待 Host 回执超时",
    observedAt: new Date().toISOString(),
  };
}

function terminalFromState(client: CommandWaitClient, requestId: CommandRequestId): CommandReceipt | undefined {
  const entry = client.getState?.()?.commands?.[requestId];
  if (entry === undefined) return undefined;
  if (entry.status === "completed" || entry.status === "failed" || entry.status === "rejected" || entry.status === "outcome_unknown") {
    return entry;
  }
  return undefined;
}

export function waitForCommandReceipt(
  client: CommandWaitClient,
  requestId: CommandRequestId,
  timeoutMs: number | null = RECEIPT_TIMEOUT_MS,
): Promise<CommandReceipt> {
  const existing = terminalFromState(client, requestId);
  if (existing !== undefined) return Promise.resolve(existing);
  return new Promise((resolve) => {
    let settled = false;
    const settle = (receipt: CommandReceipt) => {
      if (settled) return;
      settled = true;
      if (timer !== undefined) clearTimeout(timer);
      unsubscribe();
      offState?.();
      resolve(receipt);
    };

    // Reading the state once is not enough, and this used to be all it did. The
    // reducer can move an in-flight command to a terminal status with no
    // matching `command.receipt` event — re-bootstrap, resync, a runtime epoch
    // change, runtime loss — and if that happens after this line, nothing ever
    // settled: the caller waited forever, with no error and no way back. The
    // comment on `waitReceipt` (hostError) says exactly this; this copy of the
    // wait did not inherit the fix. Live at DiagnosticsPage's install/recheck,
    // runtimeEnsure, and HomePage.
    const offState =
      typeof client.onState === "function"
        ? client.onState(() => {
            const raced = terminalFromState(client, requestId);
            if (raced !== undefined) settle(raced);
          })
        : undefined;

    const unsubscribe = client.subscribe({ scope: "command", requestId }, (event) => {
      if (event.kind !== "command.receipt" || event.receipt === undefined) return;
      settle(event.receipt);
    });

    const timer =
      timeoutMs === null
        ? undefined
        : setTimeout(() => settle(unknownReceipt(requestId)), timeoutMs);

    const raced = terminalFromState(client, requestId);
    if (raced !== undefined) settle(raced);
  });
}

export async function resumeHistoryEntry(
  client: {
    command(name: "session.resume", input: { threadId: SessionHistoryEntry["threadId"] }): Promise<{ requestId: CommandRequestId }>;
    subscribe: CommandWaitClient["subscribe"];
  },
  entry: SessionHistoryEntry,
  gate: ResumeGenerationGate,
): Promise<{ generation: number; receipt: CommandReceipt }> {
  const generation = gate.next();
  const handle = await client.command("session.resume", { threadId: entry.threadId });
  const receipt = await waitForCommandReceipt(client, handle.requestId);
  return { generation, receipt };
}

export async function ensureSelectedSessionActive(
  client: StudioClient,
  target: {
    readonly activeSessionId?: SessionId;
    readonly selectedSessionId?: SessionId;
    readonly selectedThreadId?: ThreadId;
  },
): Promise<void> {
  if (target.selectedSessionId === undefined || target.activeSessionId === target.selectedSessionId) return;
  if (target.selectedThreadId === undefined) {
    throw { code: "INVALID_ARGUMENT", message: "当前历史会话缺少 threadId，无法自动恢复后发送。" };
  }
  const handle = await client.command("session.resume", { threadId: target.selectedThreadId });
  const snapshot = await waitReceipt<OperatorStateSnapshot>(client, handle.requestId, 30_000);
  if (snapshot.sessionId !== target.selectedSessionId) {
    throw {
      code: "STATE_VERSION_CONFLICT",
      message: `恢复后的会话与发送目标不一致：期望 ${target.selectedSessionId}，实际 ${snapshot.sessionId}。`,
    };
  }
}

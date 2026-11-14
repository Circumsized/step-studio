import { createInterface } from "node:readline";

// Mirrors the real `step --mode rpc` event shapes so the adapter's mapping is
// exercised against upstream's protocol, not an invented one.
//
// A turn ends with `session_settled` (`rpc-session-settle.ts` in the Runtime),
// never `agent_settled` — a name that appears nowhere in upstream. This fixture
// used to emit the invented one, so it agreed with an adapter that was matching
// the invented one, and both were wrong together. The real CLI's
// `prompt_result` frame is emitted too, carrying the id the prompt was sent
// under, because that is what the adapter now also settles on.
const rl = createInterface({ input: process.stdin });
const write = value => process.stdout.write(JSON.stringify(value) + "\n");
const userMessage = text => ({ role: "user", content: [{ type: "text", text }], timestamp: Date.now() });

// A catalog with two providers so selector resolution has something to resolve.
const CATALOG = {
  models: [
    { id: "stepfun-cn/step-3.7-flash", name: "Step 3.7 Flash", provider: "local-gateway" },
    { id: "zed/claude-haiku-5-5", name: "Claude Haiku 5.5", provider: "local-gateway" },
  ],
};

// Dialogs the fake is holding open, keyed by the id it issued. The real CLI
// blocks the agent on a dialog method until the client answers with the
// matching id, so the fake has to wait the same way.
const pendingResponses = new Map();
const waitForResponse = id => new Promise(resolve => pendingResponses.set(id, resolve));

const state = {
  sessionId: "fake-session",
  model: { provider: "local-gateway", modelId: "stepfun-cn/step-3.7-flash" },
  thinkingLevel: "off",
  entries: [],
};

function appendEntry(type, extra) {
  const entry = { id: `e${state.entries.length + 1}`, parentId: state.entries.at(-1)?.id ?? null, timestamp: Date.now(), type, ...extra };
  state.entries.push(entry);
  return entry;
}

rl.on("line", line => {
  const message = JSON.parse(line);
  if (message.type === "get_state") {
    write({
      type: "response",
      command: "get_state",
      id: message.id,
      success: true,
      data: {
        sessionId: state.sessionId,
        isStreaming: false,
        isCompacting: false,
        pendingMessageCount: 0,
        model: { id: state.model.modelId, provider: state.model.provider },
        thinkingLevel: state.thinkingLevel,
      },
    });
  }
  if (message.type === "get_available_models") {
    write({ type: "response", command: "get_available_models", id: message.id, success: true, data: CATALOG });
  }
  if (message.type === "get_available_thinking_levels") {
    write({ type: "response", command: "get_available_thinking_levels", id: message.id, success: true, data: { levels: ["off", "low", "high"] } });
  }
  if (message.type === "set_model") {
    // Validate like the real CLI, so a mis-split selector fails here too.
    const known = CATALOG.models.some((m) => m.provider === message.provider && m.id === message.modelId);
    if (!known) {
      write({ type: "response", command: "set_model", id: message.id, success: false, error: `Model not found: ${message.provider}/${message.modelId}` });
      return;
    }
    state.model = { provider: message.provider, modelId: message.modelId };
    write({ type: "response", command: "set_model", id: message.id, success: true });
  }
  if (message.type === "set_thinking_level") {
    state.thinkingLevel = message.level;
    write({ type: "response", command: "set_thinking_level", id: message.id, success: true });
  }
  if (message.type === "get_entries") {
    write({ type: "response", command: "get_entries", id: message.id, success: true, data: { entries: state.entries, leafId: state.entries.at(-1)?.id ?? null } });
  }
  if (message.type === "fork") {
    write({ type: "response", command: "fork", id: message.id, success: true, data: { text: "", cancelled: false } });
  }
  if (message.type === "steer" || message.type === "follow_up") {
    write({ type: "response", command: message.type, id: message.id, success: true });
  }
  if (message.type === "prompt") {
    const text = String(message.message ?? "");
    // `/usage` is a slash command, not a message: the CLI dispatches it and the
    // panel comes back on the fire-and-forget notify channel as ANSI text. No
    // agent turn starts, so there is no session_settled for this one.
    if (text === "/usage") {
      write({
        type: "extension_ui_request",
        id: "fake-usage",
        method: "notify",
        extensionPath: "command:usage",
        notifyType: "info",
        message: [
          "Usage",
          "[1 Overview]   2 Account",
          "",
          "365 days · All projects",
          "    DeJan     Feb     Mar   ",
          "Su  · · · · · · · · · · ",
          "Mo  · · ▒ · · · · · · · ",
          "    Less · ░ ▒ ▓ █ More",
          "",
          "Activity insights",
          "Total chats            7",
          "Active days            2",
          "Total tokens           1,234",
        ].join("\n"),
      });
      write({ type: "prompt_result", id: message.id, status: "completed" });
      write({ type: "session_settled" });
      return;
    }
    // A turn that fails at the provider: the real CLI answers `prompt_result`
    // with `status:"error"` and the error object (`message`, `provider`, `model`,
    // `retryable`, `httpStatus`). The fixture used to have no such branch, so the
    // adapter's failure path was never exercised — and the adapter read a
    // `turn_error` field that nothing ever wrote, so a failed turn settled the
    // Studio prompt as `completed {ok:true}`. Both sides were wrong together.
    if (text === "fail") {
      write({ type: "agent_start" });
      write({ type: "message_start", message: userMessage(text) });
      write({ type: "message_end", message: userMessage(text) });
      write({ type: "agent_end", willRetry: false, messageCount: 1 });
      write({ type: "prompt_result", id: message.id, agentInvoked: true, status: "error",
        error: { message: "provider said no", provider: "fake-gateway", model: "fake-model", retryable: false, httpStatus: 402 } });
      write({ type: "session_settled" });
      return;
    }
    // Mid-turn death: no session_settled will ever follow.
    if (text === "die") {
      setTimeout(() => process.exit(3), 5);
      return;
    }
    // Compaction is Runtime-initiated, not a Host command.
    if (text === "compact") {
      write({ type: "compaction_start", reason: "auto" });
      setTimeout(() => write({ type: "compaction_end", reason: "auto", aborted: false, willRetry: false }), 5);
      setTimeout(() => write({ type: "prompt_result", id: message.id, status: "completed" }), 5);
      setTimeout(() => write({ type: "session_settled" }), 10);
      return;
    }
    // An approval round trip: a status line that lands without blocking, then
    // a confirm dialog that holds the turn open until the client answers it.
    if (text === "approve") {
      write({ type: "agent_start" });
      const userEntry = appendEntry("message", { message: userMessage(text) });
      write({ type: "message_start", message: userMessage(text) });
      write({ type: "message_end", message: userMessage(text) });
      write({ type: "extension_ui_request", id: "fake-ui-status", method: "setStatus", statusKey: "fake", statusText: "waiting for approval" });
      write({ type: "extension_ui_request", id: "fake-ui-1", method: "confirm", title: "Allow this command?", message: "delete the build directory", timeout: 5000 });
      // The real CLI auto-resolves a timed-out dialog; the fake does the same
      // after 10s so a broken answer path fails the test instead of hanging it.
      const timedOut = new Promise((resolve) => setTimeout(() => resolve(null), 10000));
      Promise.race([waitForResponse("fake-ui-1"), timedOut]).then((answer) => {
        const confirmed = answer?.confirmed === true;
        const verdict = confirmed ? "approved" : "denied";
        const assistantEntry = appendEntry("message", {
          message: { role: "assistant", content: [{ type: "text", text: verdict }], stopReason: "stop", timestamp: Date.now() },
        });
        write({ type: "message_start", message: { role: "assistant", content: [], stopReason: "pending", timestamp: Date.now() } });
        write({ type: "message_update", usage: {}, assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta: verdict } });
        write({ type: "message_end", message: state.entries.find((entry) => entry.id === assistantEntry.id).message });
        write({ type: "turn_end", messageRole: "assistant", toolResultCount: 0 });
        write({ type: "agent_end", willRetry: false, messageCount: 3 });
        write({ type: "prompt_result", id: message.id, status: "completed" });
        write({ type: "session_settled" });
      });
      return;
    }
    // A long gap between the last delta and the receipt, so a test can tell
    // streaming from buffering without depending on machine speed.
    // `null` means the turn never settles on its own: only an abort ends it.
    const settleAfter = text === "slow" ? 1500 : text === "hang" ? null : 70;
    write({ type: "agent_start" });
    const userEntry = appendEntry("message", { message: userMessage(text) });
    write({ type: "message_start", message: userMessage(text) });
    write({ type: "message_end", message: userMessage(text) });
    const assistantEntry = appendEntry("message", {
      message: { role: "assistant", content: [{ type: "text", text: "hello!" }], stopReason: "stop", timestamp: Date.now() },
    });
    write({
      type: "message_start",
      message: { role: "assistant", content: [], stopReason: "pending", timestamp: Date.now() },
    });
    for (const delta of ["he", "llo", "!"]) {
      setTimeout(
        () =>
          write({
            type: "message_update",
            usage: {},
            assistantMessageEvent: { type: "text_delta", contentIndex: 0, delta },
          }),
        10,
      );
    }
    // A tool call and its result, the way the real CLI streams them.
    write({ type: "message_update", usage: {}, assistantMessageEvent: { type: "toolcall_start", contentIndex: 0, id: "call-1", toolName: "list_directory" } });
    write({ type: "message_update", usage: {}, assistantMessageEvent: { type: "toolcall_end", contentIndex: 0, toolCall: { type: "toolCall", id: "call-1", name: "list_directory", arguments: { path: "." } } } });
    write({ type: "tool_execution_start", toolCallId: "call-1", toolName: "list_directory", args: { path: "." } });
    write({ type: "tool_execution_end", toolCallId: "call-1", toolName: "list_directory", result: { content: [{ type: "text", text: "dir apps/" }], isError: false } });
    setTimeout(
      () =>
        write({
          type: "message_end",
          message: state.entries.find((entry) => entry.id === assistantEntry.id).message,
        }),
      60,
    );
    if (settleAfter !== null) {
      setTimeout(() => {
        write({ type: "turn_end", messageRole: "assistant", toolResultCount: 0 });
        write({ type: "agent_end", willRetry: false, messageCount: 3 });
        write({ type: "prompt_result", id: message.id, status: "completed" });
        write({ type: "session_settled" });
      }, settleAfter);
    }
  }
  if (message.type === "extension_ui_response") {
    const resolve = pendingResponses.get(message.id);
    if (resolve) {
      pendingResponses.delete(message.id);
      resolve(message);
    }
  }
  if (message.type === "abort") {
    write({ type: "prompt_result", id: message.id, status: "aborted" });
    write({ type: "session_settled" });
  }


});

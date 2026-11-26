// Core regression list. One source of truth for `npm run check`, `npm run test:core`
// and CI. Keep it on end-user workflows; page details and fixtures belong to the
// package suites, not the default gate.
//
// Paths are relative to the repository root. Node suites run against compiled output
// (`dist/`), renderer suites run through Vitest against sources.

export const CORE_NODE_SUITES = [
  // Bridge contracts, client receipts, and both transport adapters.
  "packages/studio-protocol/dist/test/protocol.test.js",
  // Step Code's Studio Bridge adapter, driven by the real studio-host client.
  // Skips when no Step CLI is installed; exercises the real binary when one is.
  "packages/studio-protocol/dist/test/conversation.test.js",
  "packages/client/dist/test/reducer.test.js",
  "packages/client/dist/test/receipt-selectors.test.js",
  "packages/testkit/dist/test/transport-contract.test.js",
  // Opening, recovering, browsing and deleting conversations.
  "packages/studio-host/dist/test/backend-lifecycle.test.js",
  // The Rust adapter driven by the TypeScript client: the cross-language half of
  // what `runtime-stepcode` used to cover. Skips without a Step CLI.
  "packages/studio-host/dist/test/real-runtime-adapter.test.js",
  "packages/studio-host/dist/test/conversation-open.test.js",
  "packages/studio-host/dist/test/session-catalog.test.js",
  "packages/studio-host/dist/test/session-delete-service.test.js",
  "packages/host-client-api/dist/test/conversation-open.test.js",
  "packages/host-client-api/dist/test/facade-history.test.js",
  // Which model catalog the composer sees: the Runtime's, or the config file's.
  "packages/host-client-api/dist/test/runtime-models.test.js",
  "packages/host-client-api/dist/test/session-delete-command.test.js",
  // Catalog refresh: the update path, and the trust boundary it crosses. Its
  // fetch is stubbed, so it is hermetic.
  "packages/host-client-api/dist/test/model-adaptation.test.js",
  // Runtime resolution and the trust gate: which runtime earns a controller,
  // and which executable actually gets spawned.
  // Local file writes, Electron boundaries, installation and update recovery.
  // The Tauri shell's IPC sender: command mapping, the single shared event
  // listener, and the rejection re-wrap that keeps Electron's error shape.
  "apps/desktop-tauri/dist/test/tauri-sender.test.js",
  "apps/desktop-tauri/dist/test/command-table.test.js",
  // The chrome and terminal bridges. Same reason as `command-table`: a renamed
  // `#[tauri::command]` or a moved argument key compiles, typechecks, and then
  // fails only at runtime, so the JS and Rust sources are read back and aligned
  // in both directions. The terminal one also pins the single-`listen` fan-out.
  "apps/desktop-tauri/dist/test/chrome-bridge.test.js",
  "apps/desktop-tauri/dist/test/terminal-bridge.test.js",
  "packages/runtime-installer/dist/test/installer.test.js",
  "packages/runtime-installer/dist/test/update-v2.test.js",
  "packages/runtime-installer/dist/test/update-index.test.js",
  // Version ordering, including the semver 11.4.3 numeric-below-alphanumeric rule.
  "packages/runtime-installer/dist/test/version-compare.test.js",
];

export const CORE_RENDERER_SUITES = [
  "src/composer/ChipComposer.test.tsx",
  "src/composer/dispatch.test.ts",
  "src/composer/serialize.test.ts",
  // The service-tier trigger, the minimap filter menu and the SSH connection
  // dialog. All three are end-user surfaces whose accessible names changed
  // under the components while the tests stayed behind, so the default gate was
  // green on suites that were red.
  "src/composer/ServiceTierPicker.test.tsx",
  "src/conversation/conversationStore.test.ts",
  "src/conversation/ConversationMinimap.test.tsx",
  // The mermaid config that decides whether model-produced SVG reaches
  // `dangerouslySetInnerHTML` sanitised. Its assertions are the only thing that
  // fails if someone drops `htmlLabels` from `secure`.
  "src/conversation/mermaidQueue.test.ts",
  "src/conversation/useConversation.test.ts",
  "src/InteractionDeck.test.tsx",
  "src/sessionLifecycle.test.ts",
  "src/HistoryPage.test.tsx",
  "src/settings/DesktopUpdateRecovery.test.tsx",
  "src/workspaces/ExecutionTargets.test.tsx",
];

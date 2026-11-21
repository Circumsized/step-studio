# Architecture

OMP Studio is a desktop console in front of a **patched OMP Runtime**. The
GUI does not parse TUI text, ANSI, or key macros. Control goes through a
typed Studio Bridge.

```
Renderer (apps/renderer)
  → @step-studio/client          StudioClient + conversation reducer
  → transport-desktop           preload `window.stepStudio`
  → apps/desktop                ipc.ts
  → @step-studio/host-client-api product facade
        ├─ Host-owned: session catalog, archive, models.yml,
        │              skills scan, git, workspace registry
        └─ Runtime Bridge: studio-host
              → overlay packages/coding-agent/src/studio/**
```

## Packages

| Package | Responsibility |
|---|---|
| `@step-studio/studio-protocol` | Bridge frames, command kinds, validators |
| `@step-studio/client-contract` | Renderer-visible queries, commands, read models |
| `@step-studio/client` | Client state machine, conversation reducer |
| `@step-studio/host-client-api` | Facade: query/command → Host or Runtime |
| `@step-studio/studio-host` | Runtime process, Bridge, ledger, catalog, telemetry |
| `@step-studio/transport-desktop` | Fixed IPC channel names and validation |
| `@step-studio/transport-web` | Optional local web transport |
| `@step-studio/runtime-installer` | Signed artifact install / rollback |
| `@step-studio/platform*` | Single-instance lock, Job Object, private endpoint |
| `@step-studio/testkit` | Shared fixtures and contract suites |

Authoritative lists:

- Client operations: `packages/client-contract/src/operations.ts`
- Runtime command `kind`: `packages/studio-protocol/src/contracts/commands.ts`
- Facade dispatch: `packages/host-client-api/src/facade.ts`

## Invariants

- When prose and types disagree, **contracts win**.
- The renderer may call Host domain APIs only. It must never receive bridge
  tokens, process handles, or OMP session filesystem paths.
- Unknown mutations fail closed.
- `accepted` is not success. Only a terminal receipt commits a terminal
  outcome.
- Runtime loss fences the old epoch and maps unresolved accepted work to
  `outcome_unknown`.
- PTY bytes are not a control protocol.

## Runtime pin

`omp-patch/upstream.json` pins [oh-my-pi](https://github.com/can1357/oh-my-pi).
Studio-owned code lives in `omp-patch/overlay/`. Edits to upstream files are
four seam patches. See [`omp-patch/README.md`](../omp-patch/README.md).

`omp.exe` is the patched CLI/Runtime. `OMP Studio.exe` is the Electron app.

## Historical notes

[`doc/BACKEND_FOUNDATION.md`](../doc/BACKEND_FOUNDATION.md) and
[`doc/FRONTEND_INTEGRATION.md`](../doc/FRONTEND_INTEGRATION.md) describe the
phase before the renderer landed. They are kept as archaeology. Prefer this
file and the TypeScript contracts.

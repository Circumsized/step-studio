<p align="center">
  <img src="icon.png" width="96" height="96" alt="Step Studio">
</p>

<h1 align="center">Step Studio</h1>

<p align="center">
  <strong>A desktop console for Step Code's <code>step</code> CLI</strong><br>
  Typed Studio Bridge · Tauri (Rust) shell · Windows / macOS
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT"></a>
  <a href="CHANGELOG.md"><img src="https://img.shields.io/badge/version-0.1.17-informational.svg" alt="0.1.17"></a>
  <a href="docs/getting-started.md"><img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS-0078D4.svg" alt="Windows | macOS"></a>
  <a href="package.json"><img src="https://img.shields.io/badge/node-%3E%3D22-339933.svg" alt="Node 22+"></a>
</p>

---

Step Studio puts [Step Code](https://github.com/can1357/oh-my-pi)'s `step` CLI in a desktop window.
Sessions, approvals, Agent Hub, Skills, models, Git and the terminal are all driven over a
**typed Studio Bridge** — no TUI scraping, no ANSI parsing, no key macros. The control plane is
semantic commands, not screen reading.

The whole chain is Rust: a Tauri shell starts `step-studio-host` (daemon), which starts
`step-studio-runtime` (adapter), which speaks JSONL to `step --mode rpc` over stdio.

<p align="center">
  <img src="docs/images/workbench.png" alt="Step Studio workbench" width="920">
</p>

## The chain

```
step-studio-desktop-tauri.exe    Tauri shell (Rust) + React renderer
  └─ step-studio-host.exe        daemon: endpoint, auth, lifecycle, adapter supervision
       └─ step-studio-runtime.exe adapter: translates Studio commands into step's JSONL
            └─ step              Step Code CLI (--mode rpc)
```

The three executables **must sit in one directory**: the shell looks for the daemon beside
itself, and the daemon uses the same order to find the adapter. An installed copy satisfies
this; a hand-copied set that misses one prints
`[bridge] no step-studio-host binary found` and does nothing else.

The adapter resolves `step` from `PATH`, or from wherever `STEP_STUDIO_STEP_CMD` points.
**Without it the daemon and adapter still start and the HMAC handshake still completes** —
only the call into the CLI fails.

## Quick start

Node.js 22+ and Rust.

```powershell
git clone --recurse-submodules https://github.com/Circumsized/step-studio.git
cd step-studio
npm install
cd apps/desktop-tauri
npm run tauri:dev
```

`tauri:dev` builds the preload bridge and the renderer, then opens the window. The first run
takes a few minutes. For a distributable installer, swap the last line for `npm run tauri:build`.

Cross-building Windows from Linux needs `--target x86_64-pc-windows-msvc --runner cargo-xwin`;
the full prerequisites and the traps are in
[`apps/desktop-tauri/src-tauri/BUNDLE-TARGETS.md`](apps/desktop-tauri/src-tauri/BUNDLE-TARGETS.md).

For what to run and capture once you have it on Windows, see
[`docs/windows-real-machine-test.md`](docs/windows-real-machine-test.md).

## What it does

- **Conversation workbench** — composer (chips, `@` mentions, slash, images), tool cards, session changes, subagents, Plan / Vibe / approvals.
- **Sessions and projects** — local workspace registry, history, archive. The renderer sees opaque ids, never native absolute paths.
- **Agent Hub / Skills / models** — inventory and config go through the Host. Surfaces without a read model stay disabled instead of filling with fake data.
- **Native capabilities** — Git, file tree, terminal PTY, notifications, open-in-browser. The control plane stays semantic commands, not glued-together shell.
- **Managed Runtime** — a Rust daemon plus an adapter speaking `step --mode rpc`. Artifacts are Ed25519-verified.
- **SSH workspaces (dev preview)** — pick a local or remote workspace from the project/branch bar; boundaries and unaccepted cases are in [the SSH notes](docs/ssh-workspaces.md).

<p align="center">
  <img src="docs/images/home.png" alt="Home" width="920">
  <img src="docs/images/hub.png" alt="Agent Hub" width="920">
  <img src="docs/images/models.png" alt="Models and providers" width="920">
</p>

## Layout

```
apps/desktop-tauri/   Tauri shell (Rust) + preload bridge; installer config lives here too
apps/desktop-tui/     terminal surface, shares the store readers with the desktop shell
apps/renderer/        Vite + React workbench
packages/             11 @step-studio/* packages: protocol, client, transport, platform, testkit
crates/               Rust: studio-bridge (frames + handshake), studio-host (daemon),
                      step-studio-runtime (adapter), step-studio-sessions, step-studio-peer-uid
omp-patch/            legacy layer: oh-my-pi submodule + overlay + seam patches (the live runtime is step)
docs/                 user and developer docs
```

## Protocol invariants

When prose and types disagree, `packages/studio-protocol` and `client-contract` win.

- The renderer must never receive bridge tokens, process handles, or Step session paths.
- Unknown mutations fail closed; `accepted` is not success.
- Runtime loss fences the old epoch and maps unresolved accepted work to `outcome_unknown`.
- The handshake is mutual: the verifier picks the challenge, and both sides prove they hold the token.

## Status

Stated honestly, what is verified and what is not:

| Item | Status |
| --- | --- |
| Windows exe and NSIS installer | **Produced**: `Step Studio_0.1.0_x64-setup.exe`, carrying the shell, daemon and adapter |
| Cross-platform build | A Linux host cross-builds Windows (`cargo-xwin`); the host target also produces a `.deb` |
| Full gate | `npm run check`, `cargo test --workspace` (except one out-of-gate diagnostic test) and `cargo clippy` all green |
| Running on real Windows | **Not verified** — whether the shell brings up WebView2, the named-pipe handshake, and the adapter launching the CLI all need a Windows machine |
| `msi` installer | Cannot be produced here (Tauri skips it outright on Linux); needs a Windows host or wine |
| WebView2 | Built into Windows 11; Windows 10 needs a one-time Evergreen Bootstrapper install |

## Documentation

| Document | Contents |
| --- | --- |
| [docs/getting-started.md](docs/getting-started.md) | Run and build from source |
| [docs/development.md](docs/development.md) | Inner loop, preview mode |
| [docs/architecture.md](docs/architecture.md) | Package responsibilities and protocol invariants |
| [docs/windows-real-machine-test.md](docs/windows-real-machine-test.md) | Windows real-machine test runbook |
| [AGENTS.md](AGENTS.md) | Collaborator conventions |
| [CHANGELOG.md](CHANGELOG.md) | User-facing changes |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). There is one check entry point, `npm run check`, sliced by scope:

```bash
npm run check -- --fast          # architecture constraints + selector rules + compile
npm run check -- --only renderer # compile the renderer dependency closure, then its core tests
npm run check                    # architecture + types + core tests
```

## License

[MIT](LICENSE). Third-party notices are in [NOTICE](NOTICE).

`omp-patch/` keeps the overlay and seam patches against upstream
[oh-my-pi](https://github.com/can1357/oh-my-pi), whose copyright and license are recorded in
[NOTICE](NOTICE). Everything else in this repository is independent work.

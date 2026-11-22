<p align="center">
  <img src="icon.png" width="96" height="96" alt="Step Studio">
</p>

<h1 align="center">Step Studio</h1>

<p align="center">
  <strong>Desktop console for the step CLI (Step Code)</strong><br>
  Typed Studio Bridge · Tauri workbench · Windows / macOS
</p>

<p align="center">
  <a href="README.md">中文</a>
  ·
  <a href="docs/README.md">Docs</a>
  ·
  <a href="CHANGELOG.md">Changelog</a>
  ·
  <a href="https://github.com/can1357/oh-my-pi">oh-my-pi</a>
</p>

<p align="center">
  <a href="https://github.com/Circumsized/step-studio/actions/workflows/ci.yml"><img src="https://github.com/Circumsized/step-studio/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT"></a>
  <a href="CHANGELOG.md"><img src="https://img.shields.io/badge/version-0.1.17-informational.svg" alt="0.1.17"></a>
  <a href="docs/getting-started.md"><img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS-0078D4.svg" alt="Windows | macOS"></a>
  <a href="package.json"><img src="https://img.shields.io/badge/node-%3E%3D22-339933.svg" alt="Node 22+"></a>
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX%20DO-Community-FFB003.svg" alt="LINUX DO"></a>
</p>

Step Studio is a desktop shell for Step Code's `step` CLI. Sessions, approvals, Agent Hub, and the workspace are driven through a typed Studio Bridge — not by scraping TUI text, ANSI, or key macros.

<p align="center">
  <img src="docs/images/workbench.png" alt="Step Studio workbench: new conversation welcome, activity heatmap, and composer" width="920">
</p>

The shell is Tauri (Rust) over a React renderer; the runtime side is a Rust daemon plus an adapter for `step --mode rpc`. See the [changelog](CHANGELOG.md). Windows x64 / ARM64 and macOS Apple Silicon downloads are available from [GitHub Releases](https://github.com/Circumsized/step-studio/releases). The Mac application is ad hoc signed and not notarized.

## Why this workbench

Skills, usage, providers, capabilities, and Git live in one window — no hopping between a TUI and a pile of web pages.

### 1. Fast-select Skills sidebar

Open the Skills & Plugins drawer from the left rail. Skills are grouped by project / global / built-in. Click **Add** to drop a skill into the current draft; `/skill:` capsules in the transcript show what you already used.

<p align="center">
  <img src="docs/images/skills.png" alt="Skills sidebar grouped by project, global, and built-in" width="920">
</p>

### 2. Usage and stats that are actually readable

The home page has Token charts you can switch by year / month / week / day, plus a heatmap. The conversation header opens turn cost, cache hits, TPS, and sub-agent spend; Agent Hub repeats the same numbers per child.

<p align="center">
  <img src="docs/images/home.png" alt="Home page Token chart and heatmap" width="432">
  &nbsp;
  <img src="docs/images/telemetry.png" alt="Conversation Token usage popover" width="432">
</p>

### 3. Unified provider management

The model-config page keeps Anthropic, OpenAI, OpenRouter, local LM Studio, and friends in one list — toggles, endpoints, and `modelProviderOrder` on the same screen. The Roles tab assigns models and thinking to `@default` / `@plan` / `@task`.

<p align="center">
  <img src="docs/images/models.png" alt="Model config: unified provider list" width="432">
  &nbsp;
  <img src="docs/images/roles.png" alt="Role config: models per task" width="432">
</p>

### 4. Capabilities center

Skills, Plugins, MCP, and Slash Commands share one page for toggles, probes, and folders. Agent Hub watches the main session and every sub-agent’s status, cost, and context window.

<p align="center">
  <img src="docs/images/capabilities.png" alt="Capabilities center: Skills, Plugins, MCP, Slash" width="432">
  &nbsp;
  <img src="docs/images/hub.png" alt="Agent Hub: sub-agent list and usage" width="432">
</p>

### 5. Git, in the same window

The right-hand Git panel stages, diffs, and commits next to the conversation. Fetch / Pull / Push and the commit graph stay beside the thread.

<p align="center">
  <img src="docs/images/git.png" alt="Workbench Git panel: diff, commit, and graph" width="920">
</p>

> [!TIP]
> **Installation & Getting Started:** Windows users can download the latest installer directly from [GitHub Releases](https://github.com/Circumsized/step-studio/releases), or run from source following the instructions below. Please file any bugs or suggestions as [Issues](https://github.com/Circumsized/step-studio/issues).
>
> **macOS (Apple Silicon, macOS 13+):** when a release ships `Step-Studio-<version>-macos-arm64.dmg`, open it and drag Step Studio into Applications. It is an ad hoc signed, not yet notarized preview: macOS blocks the first launch, so choose "Open Anyway" in System Settings › Privacy & Security, or run `xattr -dr com.apple.quarantine "/Applications/Step Studio.app"`. You can also run from source, see [docs/getting-started.md](docs/getting-started.md).

## What it does

- **Conversation workbench** — composer (chips, `@` mentions, slash commands, images), tool cards, session changes, subagents, Plan / Vibe / approvals.
- **Sessions and projects** — local workspace registry, history, archive. The renderer sees opaque ids, never native absolute paths.
- **Agent Hub / Skills / models** — inventory and config go through the Host. Surfaces without a read model stay disabled instead of filling with fake data.
- **Desktop capabilities** — Git, file tree, terminal PTY, notifications, open-in-browser. The control plane stays semantic commands, not glued-together shell.
- **Managed Runtime** — a Rust daemon plus an adapter speaking `step --mode rpc`. Artifacts are Ed25519-verified.

## Quick start

Node.js 22+ is required. Full steps: [docs/getting-started.md](docs/getting-started.md).

```powershell
git clone --recurse-submodules https://github.com/Circumsized/step-studio.git
cd step-studio
npm install
cd apps/desktop-tauri
npm run tauri:dev
```

`tauri:dev` builds the preload bridge and the renderer, then opens the desktop
window. The first run takes a few minutes. For a distributable installer, swap
the last line for `npm run tauri:build`; for what to run and capture on a Windows
machine, see [docs/windows-real-machine-test.md](docs/windows-real-machine-test.md).

Once the shell is up it starts the daemon (`step-studio-host`) itself, and the
daemon starts the adapter (`step-studio-runtime`). The three executables must sit
in one directory, which an installed copy satisfies. The adapter looks for the
`step` CLI on `PATH`, or wherever `STEP_STUDIO_STEP_CMD` points; without it the
daemon and adapter still start and the handshake still completes — only the call
into the CLI fails. Models and login use Step Code's own configuration. This
repository does not ship provider secrets.

The `npm run omp:*` family (install-deps / overlay:apply / keys / build:host)
belongs to the legacy `omp-patch/` layer and is unrelated to the step chain
above; see [omp-patch/README.md](omp-patch/README.md).

## Layout

```
apps/desktop-tauri      Tauri shell (Rust) + preload bridge
apps/desktop-tui       Terminal interface
apps/renderer          Vite + React workbench
packages/             protocol, Host, client, transports, platform
omp-patch/            legacy layer: oh-my-pi submodule + overlay + seam patches (the live runtime is step)
crates/               Rust: studio-bridge / studio-host / step-studio-runtime and friends
packaging/            Release signing keys and notes
ui_reference/ver1     visual reference, not product code
docs/                 user and contributor docs
```

When changing a product surface, jump from [doc/feature-index.md](doc/feature-index.md) instead of searching the whole tree.

## Architecture

```
Renderer → StudioClient → Desktop IPC → Host facade
                                    ├─ local: session catalog / Git / workspace
                                    └─ Bridge → step-studio-runtime → step --mode rpc
```

Invariants (see [docs/architecture.md](docs/architecture.md)):

- When prose and types disagree, `studio-protocol` and `client-contract` win.
- The renderer must never receive bridge tokens, process handles, or Step session paths.
- Unknown mutations fail closed; `accepted` is not success.
- Runtime loss fences the old epoch and maps unresolved accepted work to `outcome_unknown`.

## Documentation

| | |
|---|---|
| [docs/getting-started.md](docs/getting-started.md) | Run from source |
| [docs/development.md](docs/development.md) | Inner loop, preview mode, patch regen |
| [docs/architecture.md](docs/architecture.md) | Packages and invariants |
| [docs/releasing.md](docs/releasing.md) | Version, tag, Windows installer, macOS build |
| [CONTRIBUTING.md](CONTRIBUTING.md) | How to open a PR |
| [SECURITY.md](SECURITY.md) | Vulnerability disclosure |
| [CHANGELOG.md](CHANGELOG.md) | User-facing history |
| [omp-patch/README.md](omp-patch/README.md) | Overlay / seam patches |

## Contributing

Issues and PRs are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md) and [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

Do not file security issues publicly. Follow [SECURITY.md](SECURITY.md).

Chinese-language discussion and feedback happen on the [LINUX DO](https://linux.do/) community — thanks to everyone there for their support.

Defects in upstream oh-my-pi belong in [can1357/oh-my-pi](https://github.com/can1357/oh-my-pi); this repository keeps only its overlay and seam patches under `omp-patch/`. Everything else — the Tauri shell, the Rust daemon and adapter, the renderer and the protocol packages — is independent work.

## License

[MIT](LICENSE). Third-party notices: [NOTICE](NOTICE).

The pinned Runtime is [oh-my-pi](https://github.com/can1357/oh-my-pi) (MIT; Pi by Mario Zechner, omp by Can Bölük). Studio’s overlay and seam patches are independent work in this repository, also MIT.

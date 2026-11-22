# Getting started

The desktop app is a Tauri shell (`apps/desktop-tauri`) over a React renderer
(`apps/renderer`). Runtime and update catalogs use Ed25519 signatures; the
Windows Setup is not Authenticode-signed.

macOS 13 or later on Apple Silicon also runs from source. The ad hoc signed app
path is not wired up yet — `npm run pack:mac` points at a script that does not
exist in this checkout.

## Requirements

- Node.js 22 or newer, with npm
- Git (clone with submodules)
- [Bun](https://bun.sh) (OMP vendor workspace)
- For a real Runtime: MSVC Build Tools + Rust, via `npm run vendor:install-deps`
- On macOS instead: the Xcode Command Line Tools (`xcode-select --install`),
  Rust from rustup (host `aarch64-apple-darwin`), Bun 1.4.2 or newer and a
  native arm64 Node, not one running under Rosetta. `npm run vendor:install-deps`
  checks all of these before it builds anything.

You still need an OMP-compatible model provider. Studio reads the same
`models.yml` / login flow as the OMP CLI. It does not embed API keys.

## Clone

```powershell
git clone --recurse-submodules https://github.com/Circumsized/step-studio.git
cd step-studio
```

If you already cloned without submodules:

```powershell
git submodule update --init --recursive
```

## Install and run

```powershell
npm install
cd apps\desktop-tauri
npm run tauri:dev
```

`tauri:dev` runs `build.beforeDevCommand` — the preload bridge, then the Vite
dev server on port 5173 — and opens the desktop window against it. The first
start takes several minutes.

There is no `npm run preview` and `preview.cmd` is dead: both point at
`scripts/preview.mjs`, which is not in this checkout. The Electron preview they
describe is gone with the Electron shell.

The first start may take several minutes. This snapshot hides the in-app
**预览** switch (`PREVIEW_MODE_SWITCH_ENABLED=false`) and uses Host / desktop
data only. Composer, terminal, pause/resume, and approvals talk to a real Host
when one is running.

## Run with a managed Runtime

```powershell
npm install
npm run vendor:install-deps
npm run vendor:overlay:apply
npm run vendor:keys              # once per machine; writes keys under %APPDATA%\step-studio\keys
                              # (macOS: ~/Library/Application Support/step-studio/keys)
npm run vendor:build:host
cd apps\desktop-tauri
npm run tauri:dev
```

`vendor:overlay:apply` copies Studio’s overlay and seam patches into the
submodule working tree. That **will** show as modified content inside
`omp-patch/vendor/oh-my-pi`. Do not `git add` that dirty vendor tree. The
source of truth is `omp-patch/overlay/` plus `omp-patch/patches/`.

Host logs: `%APPDATA%\step-studio\logs\host-YYYY-MM-DD.log`; on macOS
`~/Library/Application Support/step-studio/logs/host-YYYY-MM-DD.log`.

## Windows installer (unsigned)

`npm run pack:win` is declared in `package.json` but `scripts/pack-win.mjs` is
not in this checkout, so it fails immediately. What does work is Tauri's own
bundler, which produces a Setup carrying the shell, the daemon and the adapter:

```powershell
cd apps\desktop-tauri
tauri build --target x86_64-pc-windows-msvc --runner cargo-xwin
```

Output: `target/x86_64-pc-windows-msvc/release/bundle/nsis/Step Studio_<version>_x64-setup.exe`.
The Setup is not Authenticode-signed; SmartScreen will warn. Cross-building from
Linux needs `cargo-xwin` for the MSVC sysroot, and the command must run from
`apps/desktop-tauri` — see
[BUNDLE-TARGETS.md](../apps/desktop-tauri/src-tauri/BUNDLE-TARGETS.md) for why
each of those is load-bearing.

To run on a Windows machine without installing, copy `step-studio-desktop-tauri.exe`,
`step-studio-host.exe` and `step-studio-runtime.exe` into one directory and start
the first one; the shell looks for the other two beside itself. What that run
should measure is in
[windows-real-machine-test.md](windows-real-machine-test.md).

## macOS app (ad hoc signed)

Not wired up in this checkout. `npm run pack:mac` points at
`scripts/pack-mac.mjs`, which does not exist, so the command fails before it
builds anything. What Tauri's own bundler produces on an Apple Silicon Mac:

```bash
cd apps/desktop-tauri
npm run tauri:build
```

Output in `target/release/bundle/`: `dmg/Step Studio_<version>_aarch64.dmg` and
`macos/Step Studio.app`. Signing, notarization and the bundle audit described in
[packaging/README.md](../packaging/README.md) are not part of that path yet.

When it is signed ad hoc and not notarized, the first launch is blocked.
Choose "Open Anyway" in System Settings › Privacy & Security, or run
`xattr -dr com.apple.quarantine "/Applications/Step Studio.app"`. Every update
changes the ad hoc signature, so macOS asks again for microphone, folder and
local network access, and the Keychain may ask to allow access once more.


## Verify

```powershell
npm run check                 # build + core regression tests
```

Full workspace tests (`npm test`), packaging tests (`npm run test:tooling`),
and Runtime patch verification (`npm run vendor:verify:patches`) are available
when those areas change. They are not required just to run the app.

## Next

- [development.md](development.md) — inner loop and patch regen
- [architecture.md](architecture.md) — what talks to what
- [releasing.md](releasing.md) — tagging, the Windows installer and the macOS build

// The three places a release reads the app version from must agree.
//
// They drifted to 0.1.17 / 0.1.0 / 0.1.0 and nothing said so, which is how the
// release chain came to advertise `Step-Studio-Setup-0.1.17-windows-x64.exe`
// while `tauri build` produced `Step Studio_0.1.0_x64-setup.exe`: the staging
// step renames whatever tauri made, so the catalog pointed at a version the
// installer did not carry.
//
// `package.json` is the source of truth — `scripts/build-update-assets-v2.mjs`
// reads it for `appVersion`, the manifest and every asset name.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));

test("the workspace, the Tauri config and package.json carry one version", async () => {
  const pkg = JSON.parse(await readFile(`${root}package.json`, "utf8"));
  const workspace = await readFile(`${root}Cargo.toml`, "utf8");
  const tauri = JSON.parse(await readFile(`${root}apps/desktop-tauri/src-tauri/tauri.conf.json`, "utf8"));

  assert.match(pkg.version, /^\d+\.\d+\.\d+$/u, "package.json version is the source of truth");
  assert.match(workspace, new RegExp(`^version = "${pkg.version.replaceAll(".", "\\.")}"$`, "mu"), "Cargo.toml [workspace.package] version");
  assert.equal(tauri.version, pkg.version, "tauri.conf.json version wins over Cargo for the installer name");
});

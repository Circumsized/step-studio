/**
 * Capture the running shell on Xvfb, one page at a time, as PNG.
 *
 * M4.1 asks for a six-page pixel-parity sign-off against omp-studio. The only
 * thing standing between a human and that judgement was the file format: the
 * references are PNG, `xwd` is the only capture tool here, and there was no
 * ImageMagick to bridge them. `xwd2png.py` closes that gap; this script drives
 * the rest of the instrument so a reviewer gets a folder of PNGs and nothing
 * else to arrange.
 *
 * Three things the first attempt at this got wrong, kept here so they are not
 * rediscovered:
 *
 *   1. `xwd -root` captures the framebuffer, not the window. With no compositor
 *      a child window is not in the root's framebuffer, so three consecutive
 *      `-root` captures came out byte-identical. The window id has to be found
 *      with `xwininfo -root -tree` and captured with `-id`.
 *   2. Waiting 25 s is not enough: GTK/WebKit under Xvfb takes 20 s or more
 *      before the shell reaches setup, so the capture was of an empty display.
 *   3. Without `STEP_STUDIO_STEP_CMD` the adapter never starts, the daemon drops
 *      the surface after 20 s, and the shell reports "Connection reset by peer".
 *
 * Usage:
 *   node scripts/capture-page.mjs --page workbench [--out DIR] [--wait MS]
 *                                 [--width N] [--height N] [--keep]
 *
 * `--page` is the route the shell should open; see PAGES below.
 */

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(import.meta.url), "..", "..");
const args = process.argv.slice(2);
const option = (name, fallback) => {
  const index = args.indexOf(name);
  return index >= 0 && args[index + 1] ? args[index + 1] : fallback;
};
const flag = (name) => args.includes(name);

const page = option("--page", "workbench");
const width = Number(option("--width", "1280"));
const height = Number(option("--height", "800"));
const waitMs = Number(option("--wait", "55000"));
const outDir = resolve(root, option("--out", "design/signoff"));
const keep = flag("--keep");

const shell = join(root, "target", "release", "step-studio-desktop-tauri");
if (!existsSync(shell)) {
  console.error(`capture-page: no shell at ${shell}; run cargo build --release -p step-studio-desktop-tauri`);
  process.exit(1);
}

const fixture = join(root, "crates", "step-studio-runtime", "tests", "fixtures", "fake-step.mjs");
const stepShim = join(tmpdir(), `capture-step-${process.pid}`);
// The adapter takes the CLI as one executable and does not split a command
// line, so `node <script>` has to arrive through a shim.
await (await import("node:fs/promises")).writeFile(stepShim, `#!/bin/sh\nexec node ${fixture} "$@"\n`, { mode: 0o755 });

const work = join(tmpdir(), `capture-work-${process.pid}`);
mkdirSync(work, { recursive: true });

const display = `:${90 + (process.pid % 500)}`;
const xvfb = spawn("Xvfb", [display, "-screen", "0", `${width}x${height}x24`, "-nolisten", "tcp"], {
  stdio: "ignore",
});
await new Promise((r) => setTimeout(r, 1500));

const sh = spawn(shell, [], {
  cwd: work,
  env: {
    ...process.env,
    DISPLAY: display,
    // The fake CLI, so the adapter has something to drive. Without it the
    // daemon closes the surface and the shell shows a connection error.
    STEP_STUDIO_STEP_CMD: stepShim,
    STEP_STUDIO_CWD: work,
    // Prints the self-check trajectory to stderr: session established, prompt
    // accepted, per-character deltas, receipt, turn completed. That trace is
    // the evidence the capture is of a working shell rather than a crash.
    STEP_STUDIO_SELF_CHECK: "1",
    // A page to land on. The shell's route table is the renderer's.
    STEP_STUDIO_ROUTE: page,
  },
  stdio: ["ignore", "ignore", "pipe"],
});

let trace = "";
sh.stderr.on("data", (chunk) => {
  trace += chunk.toString();
  for (const line of trace.split("\n").slice(-6)) {
    if (line.trim()) process.stderr.write(`[shell] ${line}\n`);
  }
  trace = trace.split("\n").pop() ?? "";
});

/** The shell's own window id, or undefined while it has none. */
function shellWindow() {
  const tree = spawnSync("xwininfo", ["-root", "-tree", "-display", display], { encoding: "utf8" });
  if (tree.status !== 0) return undefined;
  // The shell is the only child with a size near the screen; the root itself is
  // `0x` + a much larger id and matches nothing here.
  for (const line of tree.stdout.split("\n")) {
    const match = /(0x[0-9a-f]+)\s+"[^"]*"|(0x[0-9a-f]+)\s+\(/.exec(line);
    const id = match?.[1] ?? match?.[2];
    if (id && /step-studio|desktop-tauri/i.test(line)) return id;
  }
  // Fall back to the deepest child: the renderer's WebView is what carries the
  // page, and it is the only thing worth capturing.
  const ids = [...tree.stdout.matchAll(/(0x[0-9a-f]+)/g)].map((m) => m[1]);
  return ids.length > 1 ? ids[ids.length - 1] : undefined;
}

let windowId;
const deadline = Date.now() + waitMs;
while (Date.now() < deadline) {
  windowId = shellWindow();
  if (windowId) break;
  await new Promise((r) => setTimeout(r, 1000));
}
if (!windowId) {
  console.error(`capture-page: no shell window appeared within ${waitMs} ms`);
  sh.kill("SIGKILL");
  xvfb.kill("SIGKILL");
  process.exit(1);
}

// Let the page settle after the window exists: a window id appears before the
// renderer has painted, and a capture taken at that moment is a blank frame.
await new Promise((r) => setTimeout(r, 4000));

const xwdPath = join(work, "capture.xwd");
const shot = spawnSync("xwd", ["-id", windowId, "-display", display, "-out", xwdPath], { encoding: "utf8" });
if (shot.status !== 0 || !existsSync(xwdPath)) {
  console.error(`capture-page: xwd failed: ${shot.stderr}`);
  sh.kill("SIGKILL");
  xvfb.kill("SIGKILL");
  process.exit(1);
}

mkdirSync(outDir, { recursive: true });
const pngPath = join(outDir, `${page}.png`);
const converted = spawnSync("uv", ["run", "--with", "pillow", "python", join(root, "scripts", "xwd2png.py"), xwdPath, pngPath], {
  encoding: "utf8",
});
if (converted.status !== 0) {
  console.error(`capture-page: conversion failed: ${converted.stderr}`);
  sh.kill("SIGKILL");
  xvfb.kill("SIGKILL");
  process.exit(1);
}

const settled = /receipt core\.prompt -> completed|conversation\.turn\.completed/.test(trace);
console.log(converted.stdout.trim());
console.log(`capture-page: window ${windowId} on ${display}, self-check ${settled ? "completed a turn" : "did NOT complete a turn"}`);
if (!settled) {
  console.error("capture-page: the capture is of a shell that never ran a turn; treat it as a crash capture");
}

sh.kill("SIGKILL");
xvfb.kill("SIGKILL");
await new Promise((r) => setTimeout(r, 500));
if (!keep) {
  rmSync(work, { recursive: true, force: true });
  rmSync(stepShim, { force: true });
}
process.exit(settled ? 0 : 3);

/**
 * Capture one screenshot per page, for the M4.1 pixel-parity sign-off.
 *
 * Usage:
 *   node scripts/capture-page-signoff.mjs [--dark] [--lang en] [--out DIR]
 *
 * Expects a Vite dev server on :5179 (the same one `capture-readme-shots.mjs`
 * uses). Each shot is `preview-app-harness.html?preview=1&route=<id>`, so every
 * page renders with preview fixtures and no Host.
 *
 * What this is for: M4.1 要的是「六页面与 step-studio 逐页一致」，而那只有人能判。
 * 这个脚本不试图替代那个判断——它把判断的成本降到最低：一次跑出全部页面、
 * 固定视口、固定主题、固定语言、无演示标记噪声，人只需要逐张看。
 *
 * It deliberately does not diff against a reference: there is no reference image
 * set in this repo, and inventing one would make the script claim a comparison it
 * cannot make.
 */

import { chromium } from "playwright";
import { mkdir } from "node:fs/promises";
import * as path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const option = (name, fallback) => {
  const index = args.indexOf(name);
  return index >= 0 && args[index + 1] ? args[index + 1] : fallback;
};

const dark = flag("--dark");
const lang = option("--lang", "zh");
const outDir = path.resolve(root, option("--out", "docs/signoff"));
const base = process.env.SHOT_URL ?? "http://127.0.0.1:5179/preview-app-harness.html?preview=1";

// The six pages M4.1 names, plus the two the shell adds. Order is the order a
// reviewer walks the sidebar, so the diff reads top to bottom.
const PAGES = [
  ["home", "首页 / Home"],
  ["workbench", "工作台 / Workbench"],
  ["history", "历史 / History"],
  ["agent-hub", "Agent Hub"],
  ["capabilities", "能力中心 / Capabilities"],
  ["model-config", "模型配置 / Model config"],
  ["settings", "设置 / Settings"],
  ["diagnostics", "诊断 / Diagnostics"],
  ["media", "媒体 / Media"],
  ["evaluation", "评估 / Evaluation"],
];

// Workbench *states*, not routes: the reference set in
// `ui_reference/ver2/__shots/` is mostly the same page with a different panel
// open, and a route parameter cannot reach those. Each entry drives the live UI
// the way a user would — clicking the tab, opening the panel — because the state
// belongs to the app, not to the URL.
//
// `null` marks a scenario that cannot be reached without a live turn or a failed
// environment: streaming, approval and env-fail need the real runtime. They are
// listed so the gap is named rather than silently absent.
const SCENARIOS = [
  {
    // Not a distinct state: the bottom panel is open on "Step Logs" by default on
    // this route, so this captures the workbench as it first paints. Kept because
    // `light-wb-bottom.png` is a reference for exactly that, and because dropping
    // it would hide the fact that the default view is already the bottom panel.
    id: "wb-bottom",
    label: "工作台 · 底部面板（默认态）",
    reference: "light-wb-bottom.png",
    drive: async (page) => {
      await page.getByRole("tab", { name: "Step Logs" }).click();
    },
  },
  {
    id: "wb-terminal",
    label: "工作台 · 终端",
    reference: null,
    drive: async (page) => {
      await page.getByRole("tab", { name: "Terminal" }).click();
    },
  },
  {
    id: "wb-problems",
    label: "工作台 · Problems",
    reference: null,
    drive: async (page) => {
      await page.getByRole("tab", { name: "Problems" }).click();
    },
  },
  {
    id: "wb-pvlogs",
    label: "工作台 · Preview Logs",
    reference: "light-wb-preview.png",
    drive: async (page) => {
      await page.getByRole("tab", { name: "Preview Logs" }).click();
    },
  },
  {
    id: "wb-agents",
    label: "工作台 · Agents 侧栏",
    reference: "light-wb-agents.png",
    drive: async (page) => {
      await openSidePanel(page);
      await page.getByRole("tab", { name: /Agents/ }).click();
    },
  },
  {
    id: "wb-diff-split",
    label: "工作台 · Changes 分屏",
    reference: "light-wb-diff-split.png",
    drive: async (page) => {
      await openSidePanel(page);
      await page.getByRole("tab", { name: /Changes/ }).click();
      // The bottom panel is open by default and covers the lower half, so a
      // Changes capture with it up is the default view again — which is what the
      // duplicate check reported. Collapse it so the diff is the subject.
      const collapse = page.locator('button[aria-controls="bottomPanel"]');
      if ((await collapse.count()) > 0 && (await collapse.first().getAttribute("aria-expanded")) === "true") {
        await collapse.first().click();
        await page.waitForTimeout(400);
      }
    },
  },
  {
    id: "wb-git",
    label: "工作台 · Git 侧栏",
    reference: null,
    drive: async (page) => {
      await openSidePanel(page);
      await page.getByRole("tab", { name: /Git/ }).click();
    },
  },
  {
    id: "wb-preview",
    label: "工作台 · Preview 侧栏",
    reference: null,
    drive: async (page) => {
      await openSidePanel(page);
      // `t("browser.title")` — 浏览器 / Browser. Matching /Preview/ here found the
      // bottom panel's "Preview Logs" instead, so this scenario was a second copy
      // of `wb-pvlogs` with a different filename.
      await page.getByRole("tab", { name: /浏览器|Browser/ }).click();
    },
  },
  // `light-project-home.png` has no scenario here: it is the home page *inside the
  // shell* — four action cards, recent projects, recent activity — and `route=home`
  // already captures exactly that, distinct from `workbench`. The first version of
  // this entry tried to reach it by clicking a "项目与对话" button on the workbench
  // route, which matches nothing: that string is the sidebar's `aria-label` and an
  // `<h2>`, not a button. The click blocked for Playwright's 30 s timeout, the
  // `.catch()` swallowed it, and the screenshot was the workbench as it first
  // paints — byte-identical to `wb-bottom`, which the duplicate detector did catch.
  // The deeper reason it cannot work is that the `home` route has no sidebar at all
  // (see `plan.md` §19.1), so there is nothing on it to click.
  {
    id: "wb-streaming",
    label: "工作台 · 流式回合",
    reference: "light-wb-streaming.png",
    // Needs a live turn. Preview fixtures have no streaming composer state, and
    // faking one would produce a screenshot that looks like evidence of a thing
    // that never happened — the same failure as the ten identical pages.
    drive: null,
  },
  {
    id: "wb-approval",
    label: "工作台 · 审批对话框",
    reference: "light-wb-approval.png",
    drive: null,
  },
  {
    id: "wb-minimap",
    label: "工作台 · Minimap",
    reference: "light-wb-minimap.png",
    drive: null,
  },
  {
    id: "env-fail",
    label: "环境失败态",
    reference: "light-env-fail.png",
    drive: null,
  },
  {
    id: "palette",
    label: "命令面板",
    reference: "light-palette.png",
    drive: async (page) => {
      await page.keyboard.press("Control+k");
    },
  },
];

/**
 * Whether the page is healthy enough for its screenshot to mean anything.
 *
 * This exists because of one specific capture: the 2026-11-12 workbench shot,
 * recorded as a successful capture of the workbench, which converted to a PNG
 * reading "Rendererer error — the renderer hit an unexpected error". A
 * screenshot of a crash is not evidence of anything, and nothing in the
 * instrument said so.
 *
 * `pageErrors()` / `consoleMessages()` / `requests()` are native since
 * Playwright 1.56, so this needs no extra dependency — and they are the three
 * signals that would have caught it: the page error names the crash, the console
 * carries the renderer's own message, and a failed document request is why a
 * page can be blank without erroring.
 */
async function pageHealth(page) {
  // All three are async in 1.62 — they resolve to the collected arrays. Treating
  // them as sync throws a TypeError, which is how the first version of this
  // "check" managed to be both present and useless.
  const [thrown, messages, requests] = await Promise.all([
    page.pageErrors(),
    page.consoleMessages(),
    page.requests(),
  ]);
  return {
    errors: thrown.map((error) => error.message ?? String(error)),
    consoleErrors: messages.filter((message) => message.type() === "error").map((message) => message.text()),
    failed: requests
      .filter((request) => request.failure() !== null)
      .map((request) => `${request.method()} ${request.url()} — ${request.failure()?.errorText}`),
  };
}

/** Report what `pageHealth` found, and say whether the capture is trustworthy. */
function reportHealth(label, health) {
  const problems = [
    ...health.errors.map((text) => `page error: ${text}`),
    ...health.consoleErrors.map((text) => `console error: ${text}`),
    ...health.failed.map((text) => `request failed: ${text}`),
  ];
  if (problems.length === 0) return true;
  console.error(`${label} is NOT a healthy page — the screenshot is not evidence:`);
  for (const problem of problems.slice(0, 8)) console.error(`    ${problem}`);
  if (problems.length > 8) console.error(`    … and ${problems.length - 8} more`);
  return false;
}

const browser = await chromium.launch();
// 1658x846, the size every one of the 24 reference screensshots in
// `ui_reference/ver2/__shots/` is. Matching it is what makes a capture
// comparable at all: the first attempt at this comparison stalled because the
// captures were 1280x800 against 1658x846 references, a 1.3x mismatch that has
// to be reconciled by eye before any module can be lined up.
const page = await browser.newPage({ viewport: { width: 1658, height: 846 } });
await mkdir(outDir, { recursive: true });

const suffix = `${lang}${dark ? "-dark" : ""}`;
// Every capture must be a different image. The harness used to discard the
// `route` parameter when `preserveSettings=1` was set, so all ten pages captured
// as byte-identical copies of workbench — and nothing in the output said so.
// A sign-off built on that is worse than no sign-off: it looks like evidence.
const digests = new Map();
for (const [route, label] of PAGES) {
  // No `preserveSettings=1`. That flag stops the harness writing its
  // `appSettings` blob, and the default `startupPage` is `"workbench"`
  // (`appSettings.ts:78`) — so the persisted route is never consulted and every
  // page lands on workbench. A sign-off instrument wants the opposite of
  // preserved preferences: it wants the same deterministic settings on every
  // run, which is the only way two captures taken a week apart are comparable.
  //
  // `quiet=1` is the app hiding its own demo furniture (`styles/base.css`), not
  // this script reaching in with an injected stylesheet. It used to inject one
  // and match on the text "演示" — which only worked for a client willing to
  // inject CSS, so a bare `chrome-headless-shell --screenshot` got the badges,
  // and the text match was covering twelve different class combinations, so a
  // badge rendered anywhere new was simply visible.
  const url = `${base}&route=${route}&theme=${dark ? "dark" : "light"}&lang=${lang}&quiet=1`;
  // `data-qa-ready`, not `networkidle` plus a guessed sleep. The harness sets it
  // once the shell has mounted, the fonts have loaded and the entrance animations
  // have settled (`preview-app-harness-entry.tsx:142`) — which is exactly the
  // moment a screenshot is worth taking. Waiting on the network instead measured
  // how long the dev server takes to answer, and the fixed sleeps measured
  // nothing at all: a 20-image run spent 78 s, almost all of it here.
  await page.goto(url, { waitUntil: "commit" }).catch(() => undefined);
  await page.waitForSelector("[data-qa-ready]", { timeout: 15_000 }).catch(() => undefined);
  const dest = path.join(outDir, `${route}.${suffix}.png`);
  await page.screenshot({ path: dest, animations: "disabled" });
  if (!(await reportHealth(label, await pageHealth(page)))) process.exitCode = 6;
  const { createHash } = await import("node:crypto");
  const { readFile } = await import("node:fs/promises");
  const digest = createHash("sha256").update(await readFile(dest)).digest("hex");
  const seen = digests.get(digest);
  if (seen !== undefined) {
    console.error(`${label} -> ${path.relative(root, dest)}  DUPLICATE of ${seen}: the route did not take effect`);
  } else {
    digests.set(digest, route);
    console.log(`${label} -> ${path.relative(root, dest)}`);
  }
}
if (digests.size !== PAGES.length) {
  console.error(`\n${PAGES.length - digests.size} page(s) captured an image another page already produced.`);
  console.error("The sign-off is not evidence until each page is distinct.");
  process.exitCode = 4;
}

/** Make sure the right-hand panel is open, so a click on one of its tabs lands.
 *
 * The panel is open by default on this route, and clicking its toggle when it is
 * already open *closes* it — which is how the first version of these scenarios
 * captured byte-identical images: the tabs vanished from the DOM, the click threw,
 * and the screenshot was of the page as it was. So this only clicks when the
 * panel is actually shut, and reads `aria-expanded` rather than assuming. */
async function openSidePanel(page) {
  const toggle = page.locator('button[aria-controls="sidePanel"]');
  if (await toggle.count() === 0) throw new Error("no side-panel toggle in the DOM");
  if ((await toggle.first().getAttribute("aria-expanded")) !== "true") {
    await toggle.first().click();
    await page.waitForTimeout(400);
  }
}

// Workbench states. Each starts from a fresh navigation so one scenario's open
// panel cannot leak into the next.
const reachable = SCENARIOS.filter((scenario) => scenario.drive !== null);
const unreachable = SCENARIOS.filter((scenario) => scenario.drive === null);
for (const scenario of reachable) {
  await page.goto(`${base}&route=workbench&theme=${dark ? "dark" : "light"}&lang=${lang}&quiet=1`, { waitUntil: "commit" }).catch(() => undefined);
  await page.waitForSelector("[data-qa-ready]", { timeout: 15_000 }).catch(() => undefined);
  try {
    await scenario.drive(page);
    await page.waitForTimeout(700);
  } catch (error) {
    console.error(`${scenario.label} -> could not be driven: ${error.message.split("\n")[0]}`);
    process.exitCode = 5;
    continue;
  }
  const dest = path.join(outDir, `${scenario.id}.${suffix}.png`);
  await page.screenshot({ path: dest, animations: "disabled" });
  if (!(await reportHealth(scenario.label, await pageHealth(page)))) process.exitCode = 6;
  const { createHash } = await import("node:crypto");
  const { readFile } = await import("node:fs/promises");
  const digest = createHash("sha256").update(await readFile(dest)).digest("hex");
  if (digests.has(digest)) {
    console.error(`${scenario.label} -> ${path.relative(root, dest)}  DUPLICATE of ${digests.get(digest)}: the state did not change`);
    process.exitCode = 4;
  } else {
    digests.set(digest, scenario.id);
    console.log(`${scenario.label} -> ${path.relative(root, dest)}${scenario.reference ? `  [ref: ${scenario.reference}]` : ""}`);
  }
}
for (const scenario of unreachable) {
  console.log(`${scenario.label} -> skipped: needs a live turn or a failed environment${scenario.reference ? `  [ref: ${scenario.reference}]` : ""}`);
}

await browser.close();
console.log(`\n${PAGES.length} pages captured to ${path.relative(root, outDir)}/`);
console.log("Next: compare each against step-studio by eye. The script does not diff — there is no reference set in this repo.");

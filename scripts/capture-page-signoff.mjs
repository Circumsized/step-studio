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
 * What this is for: M4.1 要的是「六页面与 omp-studio 逐页一致」，而那只有人能判。
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

async function quiet(page) {
  await page.addStyleTag({
    content: `
      .startup-notice, .tip-layer, .tip-host { display: none !important; }
      .mc-page-banner, .git-notice, .hub-ro-banner { display: none !important; }
    `,
  }).catch(() => undefined);
  await page.evaluate(() => {
    document.documentElement.setAttribute("data-theme", arguments[0] ? "dark" : "light");
    for (const el of document.querySelectorAll(".chip, .tiny.muted, p.tiny.muted")) {
      const text = (el.textContent ?? "").trim();
      if (text === "演示" || text.startsWith("演示模式") || text.includes("当前是演示数据")) {
        el.style.display = "none";
      }
    }
  }, dark).catch(() => undefined);
}

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
await mkdir(outDir, { recursive: true });

const suffix = `${lang}${dark ? "-dark" : ""}`;
for (const [route, label] of PAGES) {
  const url = `${base}&route=${route}&theme=${dark ? "dark" : "light"}&lang=${lang}&preserveSettings=1`;
  await page.goto(url, { waitUntil: "networkidle" }).catch(() => undefined);
  await page.waitForTimeout(600);
  await quiet(page);
  await page.waitForTimeout(200);
  const dest = path.join(outDir, `${route}.${suffix}.png`);
  await page.screenshot({ path: dest, animations: "disabled" });
  console.log(`${label} -> ${path.relative(root, dest)}`);
}

await browser.close();
console.log(`\n${PAGES.length} pages captured to ${path.relative(root, outDir)}/`);
console.log("Next: compare each against omp-studio by eye. The script does not diff — there is no reference set in this repo.");

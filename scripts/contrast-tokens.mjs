#!/usr/bin/env node
/**
 * WCAG 2.1 contrast for the design tokens, per comparison-checklist 9.5.
 *
 * The text ramp and the accent foreground are already measured and recorded in
 * `tokens.css` itself (see the comments above `--text` and `--text-on-accent` in
 * both themes). What was missing is the status ramp — green / amber / red / blue
 * carry real text (status pills, chips, diagnostics), so they owe 4.5:1 just like
 * the text ramp does, and nobody had checked them.
 *
 * This reads the tokens straight out of `tokens.css` rather than repeating them,
 * so the numbers cannot drift from the source. Run:
 *
 *   node scripts/contrast-tokens.mjs
 *
 * Exit 0 when every pair clears its threshold, 1 otherwise — so it can gate.
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const TOKENS = join(ROOT, "apps/renderer/src/styles/tokens.css");

/** Thresholds: 4.5:1 for body text, 3:1 for large text and non-text UI. */
const AA_TEXT = 4.5;
const AA_NON_TEXT = 3;

// ---------------------------------------------------------------- parsing

/** `#rgb`, `#rrggbb`, or `rgba(r, g, b, a)` — everything else is left alone. */
function parseColour(raw) {
  const text = raw.trim();
  const hex = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(text);
  if (hex) {
    const digits = hex[1];
    const full =
      digits.length === 3
        ? digits.split("").map((d) => d + d).join("")
        : digits;
    return { rgb: [0, 2, 4].map((i) => parseInt(full.slice(i, i + 2), 16)), alpha: 1 };
  }
  const rgba = /^rgba\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*,\s*([\d.]+)\s*\)$/.exec(text);
  if (rgba) {
    return { rgb: [1, 2, 3].map((i) => parseInt(rgba[i], 10)), alpha: Number.parseFloat(rgba[4]) };
  }
  return null;
}

/** Every `{ ... }` block that declares at least one custom property. */
function readThemes() {
  const css = readFileSync(TOKENS, "utf8");
  const blocks = [];
  const re = /([^{}]+)\{([^{}]*)\}/g;
  let match;
  while ((match = re.exec(css)) !== null) {
    const body = match[2];
    if (!/--[a-z0-9-]+\s*:/.test(body)) continue;
    const tokens = new Map();
    for (const line of body.split(";")) {
      const decl = /(--[a-z0-9-]+)\s*:\s*([^;]+)/i.exec(line);
      if (decl) tokens.set(decl[1], decl[2].trim());
    }
    if (tokens.size && tokens.has("--bg")) blocks.push({ selector: match[1].trim(), tokens });
  }
  return blocks;
}

// ---------------------------------------------------------------- maths

/** WCAG relative luminance. */
function luminance([r, g, b]) {
  const channel = (value) => {
    const srgb = value / 255;
    return srgb <= 0.03928 ? srgb / 12.92 : ((srgb + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

/** Composite a translucent colour over an opaque backdrop. */
function flatten(colour, backdrop) {
  if (colour.alpha >= 1) return colour.rgb;
  return colour.rgb.map((channel, i) => Math.round(channel * colour.alpha + backdrop[i] * (1 - colour.alpha)));
}

/** WCAG 2.1 contrast ratio, 1–21. */
function ratio(foreground, background) {
  const a = luminance(foreground);
  const b = luminance(background);
  const [light, dark] = a > b ? [a, b] : [b, a];
  return (light + 0.05) / (dark + 0.05);
}

// ---------------------------------------------------------------- the pairs

/**
 * Which token is read on which background. Only pairs that actually occur: a
 * colour nobody puts text on is not a contrast question.
 */
const PAIRS = [
  // The text ramp, on both backgrounds text sits on. `tokens.css` already
  // records these; re-measuring here is the check that the record is honest.
  { fg: "--text", on: ["--bg", "--surface"], kind: "text" },
  { fg: "--text-2", on: ["--bg", "--surface", "--surface-2"], kind: "text" },
  { fg: "--text-3", on: ["--bg", "--surface", "--surface-2"], kind: "text" },
  { fg: "--text-faint", on: ["--bg", "--surface"], kind: "text" },
  // The status ramp — the part that was never measured.
  { fg: "--green", on: ["--surface", "--bg"], kind: "text" },
  { fg: "--amber", on: ["--surface", "--bg"], kind: "text" },
  { fg: "--red", on: ["--surface", "--bg"], kind: "text" },
  { fg: "--blue", on: ["--surface", "--bg"], kind: "text" },
  { fg: "--accent", on: ["--surface", "--bg"], kind: "text" },
  // Foreground on accent. `tokens.css` records why dark has to invert it.
  { fg: "--text-on-accent", on: ["--accent"], kind: "text" },
];

// ---------------------------------------------------------------- report

const themes = readThemes();
if (themes.length < 2) {
  console.error(`expected a light and a dark theme in ${TOKENS}, found ${themes.length}`);
  process.exit(1);
}

let failures = 0;
for (const theme of themes) {
  // Match the quoted value, not the attribute name: `[data-theme="light"]`
  // contains "data-theme", which an unanchored `dark|night|data-theme` test
  // reads as the dark theme.
  const name = /["']dark["']/.test(theme.selector) ? "dark" : "light";
  const page = parseColour(theme.tokens.get("--bg") ?? "#ffffff");
  const backdropOf = (token) => {
    const colour = parseColour(theme.tokens.get(token) ?? "");
    return colour ? flatten(colour, page.rgb) : null;
  };

  console.log(`\n${name} theme  (${theme.selector})`);
  console.log("  pair                                 ratio   needs  verdict");
  for (const pair of PAIRS) {
    const fg = parseColour(theme.tokens.get(pair.fg) ?? "");
    if (!fg) {
      console.log(`  ${pair.fg} — not declared in this theme`);
      continue;
    }
    for (const bgToken of pair.on) {
      const bg = backdropOf(bgToken);
      if (!bg) continue;
      const value = ratio(flatten(fg, bg), bg);
      const needs = pair.kind === "text" ? AA_TEXT : AA_NON_TEXT;
      const ok = value >= needs;
      if (!ok) failures += 1;
      console.log(
        `  ${`${pair.fg} on ${bgToken}`.padEnd(36)} ${value.toFixed(2).padStart(5)}:1  ${needs.toFixed(1)}:1  ${ok ? "pass" : "FAIL"}`,
      );
    }
  }
}

console.log(failures === 0 ? "\nevery pair clears its threshold" : `\n${failures} pair(s) below threshold`);
process.exit(failures === 0 ? 0 : 1);

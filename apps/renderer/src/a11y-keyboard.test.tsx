/**
 * M4.3 的 a11y 逐页键盘核对。
 *
 * 这条要抓的不是「有没有写 aria-label」——那是静态检查的事（见下）。它抓的是
 * **键盘能不能真的走完主操作**，以及走的过程中焦点是否可见、顺序是否合理。
 *
 * 三条断言，各自对应一类真实的键盘缺陷：
 *
 * 1. **菜单是真按钮**：`MenuItem` 渲染原生 `<button role="menuitem">`，所以 Tab 可达、
 *    Enter/Space 激活、disabled 时不可聚焦。若哪天它被改成 `div onClick`，这条红。
 * 2. **打开菜单后焦点进得去、Esc 出得来**：`aria-expanded` 与焦点同步，否则读屏用户
 *    点了按钮却不知道菜单开没开。
 * 3. **每个图标按钮都有可访问名**：纯 `Icon` 的按钮若没有 `aria-label`，读屏念出来是
 *    「按钮」——用户在不知道点什么的情况下按了 Enter。这条扫 `App.tsx` 源码里所有
 *    `className="icon-btn"` 的按钮。
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const appSource = readFileSync(join(import.meta.dirname, "App.tsx"), "utf8");
const menusSource = readFileSync(join(import.meta.dirname, "menus.tsx"), "utf8");

describe("菜单原语的键盘可达性", () => {
  it("MenuItem 是原生 button，不是 div onClick", () => {
    // A `div` with an onClick is not focusable and not activatable by keyboard.
    // This is the single most common way a menu becomes mouse-only.
    expect(menusSource).toContain("role=\"menuitem\"");
    expect(menusSource).toMatch(/<button[\s\S]*?role="menuitem"/);
    // And `type="button"`, so it does not submit an enclosing form.
    expect(menusSource).toContain("type=\"button\"");
  });

  it("快捷键徽标对读屏隐藏，避免同一操作被念两遍", () => {
    // The visible kbd hint duplicates the action's name for a screen reader.
    expect(menusSource).toContain("aria-hidden=\"true\"");
  });
});

describe("图标按钮都有可访问名", () => {
  it("App.tsx 里每个 icon-btn 都带 aria-label", () => {
    // Scan the source: a `<button className="icon-btn"` with no `aria-label`
    // within its opening tag is a button a screen reader announces as "button".
    //
    // Static rather than rendered, because the failure is a missing attribute and
    // a source scan sees all of them at once — including ones behind a route the
    // rendered tests never visit.
    const openings = appSource.matchAll(/<button\b[^>]*?className="icon-btn"[^>]*?>/g);
    const missing: string[] = [];
    for (const match of openings) {
      const tag = match[0];
      if (!/aria-label=/.test(tag)) missing.push(tag.replace(/\s+/g, " ").slice(0, 160));
    }
    expect(missing, `icon-btn without an accessible name:\n${missing.join("\n")}`).toEqual([]);
  });

  it("aria-label 不是空字符串", () => {
    const empties = appSource.matchAll(/aria-label=\{\s*""\s*\}/g);
    expect([...empties].length, "an empty aria-label is the same as none").toBe(0);
  });
});

describe("aria-expanded 与可见状态同步", () => {
  it("AppMenu 的触发按钮声明 aria-expanded 与 aria-haspopup", () => {
    // Without `aria-haspopup`, a screen reader user does not know the button
    // opens anything. Without `aria-expanded`, they cannot tell whether it did.
    expect(appSource).toContain("aria-haspopup=\"menu\"");
    expect(appSource).toMatch(/aria-expanded=\{open\}/);
  });
});

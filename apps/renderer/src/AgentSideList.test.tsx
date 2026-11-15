/**
 * M4.1 gap iii — the Agents sidebar's collapsible sub-agent group.
 *
 * Every assertion is on rendered behaviour; nothing here reads App.tsx back.
 * That is the lesson of the icon-btn guard in `a11y-keyboard.test.tsx`, whose
 * first version matched the literal `className="icon-btn"` and stayed green
 * while five real violations sat inside compound class names.
 */

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { AgentSideList, type AgentSideRow } from "./App";

/** A completion-probe sentence: all the contract offers as current activity. */
const STAGE = "checking packages/client (3/4)";

function row(over: Partial<AgentSideRow> & Pick<AgentSideRow, "key" | "name">): AgentSideRow {
  return {
    parentKey: null,
    tone: "gray",
    status: "Running",
    detail: "role · assignment",
    activity: null,
    meta: [],
    onOpen: () => undefined,
    ...over,
  };
}

/** main + two children: the shape `session.agents.list` actually returns. */
function roster(): AgentSideRow[] {
  return [
    row({ key: "main", name: "主 Agent", tone: "blue", status: "Running" }),
    row({
      key: "sub-deps",
      parentKey: "main",
      name: "deps 子 Agent",
      tone: "blue",
      status: "Running",
      detail: "Dependency Auditor · 审计依赖",
      activity: STAGE,
      meta: ["2m 47s", "12.6k tok"],
    }),
    row({
      key: "sub-lint",
      parentKey: "main",
      name: "lint 子 Agent",
      tone: "red",
      status: "Failed",
      detail: "Lint Runner · ESLint 全量扫描",
      meta: ["38s", "3.3k tok"],
    }),
  ];
}

function head(count: string): HTMLElement {
  return screen.getByRole("button", { name: new RegExp(`${count} 个子 Agent`) });
}

/**
 * One mounted tree for the whole file, re-seeded per test.
 *
 * The renderer suite runs every file through one worker pool, and one of its
 * members (`SubagentsPanel`'s "persists the disabled state") already runs at
 * ~5.8s against vitest's 5s default test timeout under full-suite load. A fresh
 * mount per assertion group is the part of this file's cost that is removable,
 * so it is removed rather than left to make that neighbour flakier.
 */
let view: ReturnType<typeof render> | undefined;

function show(rows: readonly AgentSideRow[]): void {
  const tree = <AgentSideList rows={rows} />;
  if (view === undefined) view = render(tree);
  else view.rerender(tree);
}

afterEach(() => {
  cleanup();
  view = undefined;
});

describe("Agents 侧栏的子 Agent 分组", () => {
  it("子 Agent 收在带数量的组标题下，默认展开", () => {
    show(roster());
    expect(head("2").getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("button", { name: /deps 子 Agent/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /lint 子 Agent/ })).toBeTruthy();
  });

  it("组标题是原生 button，可聚焦且在 Tab 顺序里夹在父行与子行之间", () => {
    show(roster());
    const toggle = head("2");
    expect(toggle.tagName).toBe("BUTTON");
    expect(toggle.tabIndex).toBeGreaterThanOrEqual(0);
    toggle.focus();
    expect(document.activeElement).toBe(toggle);

    // jsdom does not implement the browser's keyboard-activation step for
    // buttons, so `fireEvent.keyDown(toggle, { key: "Enter" })` toggles nothing
    // here and asserting it would be asserting a lie. What jsdom can show is
    // the part the component owns: the head is a native button sitting in the
    // tab order between the parent row and the rows it controls.
    const order: HTMLElement[] = [...document.querySelectorAll<HTMLButtonElement>("button")]
      .filter((node) => !node.hasAttribute("disabled") && node.tabIndex >= 0);
    const parent = order.indexOf(screen.getByRole("button", { name: /主 Agent/ }));
    const child = order.indexOf(screen.getByRole("button", { name: /deps 子 Agent/ }));
    expect(parent).toBeGreaterThanOrEqual(0);
    expect(parent).toBeLessThan(order.indexOf(toggle));
    expect(order.indexOf(toggle)).toBeLessThan(child);
  });

  it("点标题收起再展开：aria-expanded 与子行可见性同步，父行不受影响", () => {
    show(roster());
    fireEvent.click(head("2"));

    expect(head("2").getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("button", { name: /deps 子 Agent/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /lint 子 Agent/ })).toBeNull();
    expect(screen.getByRole("button", { name: /主 Agent/ })).toBeTruthy();

    fireEvent.click(head("2"));
    expect(head("2").getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("button", { name: /deps 子 Agent/ })).toBeTruthy();
  });

  it("aria-controls 指向真的装着子行的那个容器", () => {
    show(roster());
    const id = head("2").getAttribute("aria-controls");
    expect(id).toBeTruthy();

    const group = document.getElementById(id as string);
    expect(group).not.toBeNull();
    expect(group?.contains(screen.getByRole("button", { name: /deps 子 Agent/ }))).toBe(true);

    fireEvent.click(head("2"));
    // Collapsed and expanded point at the same node; only its visibility moves.
    expect(document.getElementById(id as string)).toBe(group);
  });

  it("子行带合同提供的字段，stage 原样渲染、不包装成工具名", () => {
    show(roster());
    const deps = screen.getByRole("button", { name: /deps 子 Agent/ });

    expect(deps.textContent).toContain("Dependency Auditor · 审计依赖");
    expect(deps.textContent).toContain("Running");
    expect(deps.textContent).toContain("2m 47s");
    expect(deps.textContent).toContain("12.6k tok");
    expect(deps.textContent).toContain(STAGE);

    // `currentTool` / `activity` have no producer in this repo, so a probe
    // sentence must not be dressed up as "Running Tool · <something>".
    expect(screen.queryByText(/Running Tool/)).toBeNull();
    expect(screen.queryByText(new RegExp(`·\\s*${STAGE}`))).toBeNull();
    expect(screen.getByText(STAGE)).toBeTruthy();
  });

  it("点子行打开该 Agent", () => {
    const onOpen = vi.fn();
    show(roster().map((entry) => (entry.key === "sub-deps" ? { ...entry, onOpen } : entry)));
    fireEvent.click(screen.getByRole("button", { name: /deps 子 Agent/ }));
    expect(onOpen).toHaveBeenCalledTimes(1);
  });

  it("只有一个子 Agent 时也走同一条组路径", () => {
    // The reference only demonstrates count >= 2, so there is nothing to match;
    // one code path beats a special case that hides a row behind a click.
    show(roster().filter((entry) => entry.key !== "sub-lint"));
    expect(head("1").getAttribute("aria-expanded")).toBe("true");
    expect(screen.getByRole("button", { name: /deps 子 Agent/ })).toBeTruthy();
  });

  it("孙 Agent 在父组里再拿一个自己的组，并随父组一起收起", () => {
    show([...roster(), row({ key: "sub-tc", parentKey: "sub-deps", name: "typecheck 子 Agent", tone: "blue", status: "Running" })]);

    expect(head("2")).toBeTruthy();
    expect(head("1")).toBeTruthy();
    expect(screen.getByRole("button", { name: /typecheck 子 Agent/ })).toBeTruthy();

    fireEvent.click(head("2"));
    expect(screen.queryByRole("button", { name: /typecheck 子 Agent/ })).toBeNull();
    // The grandchild's own head is still in the DOM, just not reachable.
    expect(document.body.textContent).toContain("1 个子 Agent");
  });

  it("父不在 roster 里的行不会消失", () => {
    show(roster().filter((entry) => entry.key !== "main"));
    expect(screen.getByRole("button", { name: /deps 子 Agent/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: /lint 子 Agent/ })).toBeTruthy();
  });

  it("没有子 Agent 时不渲染组标题", () => {
    show([row({ key: "solo", name: "独行 Agent" })]);
    expect(screen.queryByRole("button", { name: /个子 Agent/ })).toBeNull();
  });
});

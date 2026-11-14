# M4.1 六页面像素签收 — 仪器修好后的首轮结果（2026-11-14）

> 状态：🟡 仪器已修好并产出 10 张互异截图；逐模块像素判定仍要人眼（B3）。
> 本轮真正完成的是**把「不能比」变成「能比」**，并在这个过程中发现签收仪器本身一直是坏的。

## 本轮最重要的发现：签收仪器从来没工作过

`scripts/capture-page-signoff.mjs` 声称「一次跑出全部页面」。实际上它跑出的
**10 张字节完全相同**的图：

```
$ md5sum <before>/*.png | awk '{print $1}' | sort -u | wc -l
1
```

原因链：

1. harness 入口（`preview-app-harness-entry.tsx`）读 `?route=`，算出 `qaRoute`，
   但只在 `preserveSettings !== "1"` 时才写 `omp.lastRoute`；
2. 而 `capture-page-signoff.mjs` **一直带着 `preserveSettings=1`**；
3. 于是 route 被算出来然后丢掉，`appSettings` 也不写，`startupPage` 落到默认值
   `"workbench"`（`appSettings.ts:78`）；
4. 十次导航全部落在 workbench，产出十个文件名、一张图。

没有任何报错。脚本最后还打印「Next: compare each against omp-studio by eye」——
一份看起来像证据的输出。**这比没有签收更糟**：它会被当成已经看过。

已修两处，并加了不会再次静默的护栏：

- harness：`omp.lastRoute` 改为无条件写。`preserveSettings` 的本意是「保留人自己选的
  主题/密度/语言」，不该顺手把 route 也保留掉。
- 脚本：去掉 `preserveSettings=1`（签收仪器要的是每次一致的确定性设置，不是保留偏好），
  并对每张截图做 sha256 去重——任何一张与别张相同即打印 `DUPLICATE of <page>` 并以退出码
  4 结束。

## 分辨率口径已统一

参考图 `ui_reference/ver2/__shots/` 24 张**全部是 1658×846**。此前捕获是 1280×800，
`m41-capture.md` 记的「差 1.7 倍」由此而来——不先统一口径，逐模块比对无从下手。
现在视口就是 1658×846，捕获与参考同尺寸。

## XWD → PNG 的桥（`scripts/xwd2png.py`）

`m41-capture.md` 说「本机没有 ImageMagick，XWD 无法转 PNG，所以逐模块比对做不了」。
这一条现在不成立：XWD 的格式全部写在头部，不需要任何库。

```
$ uv run --with pillow python scripts/xwd2png.py capture.xwd out.png
capture.xwd: 1280x800 -> out.png
```

两个踩过的坑，都写在脚本注释里：`XWDColor` 是 **12 字节**不是 16（错了不会报错，
只会让像素整体错位、图像斜切）；`ncolors` 而不是 `colormap_entries` 决定颜色表长度
（TrueColor 显示也会写它自己用不到的 colormap）。

对 2026-11-12 那份 `design/m41-workbench.xwd` 的转换结果是：**一张「Rendererer error」
崩溃页**。也就是说 M4.1 此前连一张可用的捕获都没有。

## 本轮产出

`design/signoff/*.zh.png`，**20 张，1658×846，全部互异**（md5 去重 = 20/20）：

10 个路由页 + 10 个工作台状态。

复现：起 dev server（`cd apps/renderer && npx vite --port 5179`）后
`node scripts/capture-page-signoff.mjs --out design/signoff`。

工作台状态不是路由能到达的——参考集里大部分是**同一页开不同面板**。所以脚本用
Playwright 直接驱活 UI（点页签、开侧栏、收底栏、按 Ctrl+K），而不是给 renderer 加
查询参数。四个场景驱不动，脚本**明确跳过并说明原因**，不留静默缺口：

| 场景 | 参考图 | 为什么跳过 |
| --- | --- | --- |
| 流式回合 | `light-wb-streaming.png` | 要一个真实 turn；预览夹具没有流式 composer 状态，伪造一张会变成「看起来像证据的假图」——正是那十张相同图犯的错 |
| 审批对话框 | `light-wb-approval.png` | 同上 |
| Minimap | `light-wb-minimap.png` | 同上 |
| 环境失败态 | `light-env-fail.png` | 要一个失败的环境 |

驱活过程中脚本自己抓到三个会静默出错的地方，都已修并留注释：

- 侧栏页签在侧栏**关闭时不在 DOM 里**，而侧栏在这条路由上**默认是开的**——所以
  「先点开侧栏」的辅助函数反而把它关了，点击抛异常，截图落在原始状态。现在读
  `aria-expanded` 再决定要不要点。
- Preview 侧栏页签的中文名是**「浏览器」**（`t("browser.title")`）。用 `/Preview/`
  匹配到底栏的 "Preview Logs"，于是这个场景是 `wb-pvlogs` 换了文件名。
- 底部面板**默认就开在 Step Logs**，所以「工作台 · 底部面板」这个场景其实是默认态，
  而 Changes 场景不收起底栏就又是默认态。现在 diff 场景先收底栏，让 diff 成为主体。

## 签收链路的分层接线（2026-11-14 定）

「比 Playwright 更轻更 agent 原生」的调研结论是：**所有 agent 化都在优化驱动层与 token
效率，没有一个优化像素保真，而它们砍掉的恰好是签收最需要的**（auto-wait、跨引擎一致、
`toHaveScreenshot` 的连拍+禁动画+stylePath+mask、固定字体/locale/pinned Docker）。
所以分层接，不换引擎：

| 层 | 选型 | 理由 |
| --- | --- | --- |
| 基线出图 | **Playwright 官方 + pinned Docker**（不动） | 唯一还带着像素确定性那一套的 |
| 驱活 / 复核 | `@playwright/cli`（token 省）或 `npx playwright mcp` | 1.62.0 起 CLI 与 MCP 收进主包，消掉「agent 的 browser build 与 runner 不一致 → 失败表现成推理玄学」 |
| 判定比对 | `@blazediff/agent` + core-native | 见上文 |
| 自检断言 | **`pageErrors()` / `consoleMessages()` / `requests()`**（1.56+ 原生） | 已接进 `capture-page-signoff.mjs`，见下 |
| 极简旁路 | Rustwright 只接 Windows WebView2 | Alpha / 仅 Chromium / Node 绑定是子集，进不了基线链；但支持 `RUSTWRIGHT_CHROMIUM` 指向已有 Chromium，与「不启动第二个浏览器」对位 |
| 视觉兜底 | Midscene 的 `aiAssert`（旁路内） | AX 树判不了图标颜色/遮挡/canvas 时补一刀；产的是语义结论，**不是可 diff 的基线像素** |

### 自检断言为什么是刚需

2026-11-12 那张被记录为「捕获成功」的工作台图，转成 PNG 是**一张
"Rendererer error — the renderer hit an unexpected error" 崩溃页**。崩溃页的截图不构成
任何证据，而仪器对此一言不发。`pageErrors()` / `consoleMessages()` / `requests()` 正是
能当场抓住它的三个信号，且 1.56 起就是原生 API，不需要外挂。

已接进 `capture-page-signoff.mjs`：每张图拍完都查一遍，有问题即打印
`<label> is NOT a healthy page — the screenshot is not evidence:` 并列出手工/控制台/请求
三类失败，退出码 6。非空泛性已验证：喂一个真会抛错的页面，三条信号分别报
`unexpected error` / `renderer threw` / 请求失败计数。

踩坑：三个 API 在 1.62 **都是异步**的（resolve 成数组）。第一版按同步写，抛 TypeError，
于是这个「检查」既存在又没用——已修。

装的是 Playwright **1.62.1**，CLI/MCP 收编与 1.56 的三个断言都已具备。

## 逐场景比对（三态：齐 / 空 / 缺）

只记能从图里直接读出的。**逐模块像素判定仍要人眼**——下面是结构比对。

### Agents 侧栏：`wb-agents.zh.png` ↔ `light-wb-agents.png`

| 比对点 | 态 |
| --- | --- |
| 卡片列表 + 状态徽标 + 角色描述 | **齐**（四张卡片：名字、状态、角色/描述、时长、token、成本、文件数都在） |
| 指标行 | **齐**（`6m 12s  48.2k tok · ¥1.94  4 文件` 对 `2m 18s  42k tok  $0.94  📄2 个文件`） |
| 「当前动作 + 目标文件」行 | **齐**（2026-11-14 补）：参考是 `⚡ Write · CapabilityProbe.tsx`，位于描述与指标之间；我们原是指标行下的 `最近工具：…`——是「最近」不是「当前」，且位置在答案之后。已改为 `ag-act`（zap 图标 + 工具 + 目标）并上移到描述与指标之间 |
| 嵌套子 Agent | **缺**：参考把子 Agent 收在可展开的 `> 2 个子 Agent` 组里，子卡片带完整指标；我们用树形字符 `└` 平铺。层级都表达了，但参考那组可折叠 |
| 状态词表 | **缺**：参考 `Waiting for Agent` / `Running Tool`；我们 `Running` / `Thinking` / `Waiting for User` / `Failed`。语义方向一致，状态集与措辞不同 |
| 面板头 | **齐**（`4 个 AGENT` + Task List + × 对 `Agents 4` + Task List + ×） |
| 工作台整体骨架 | **齐**（左侧栏 + 中对话 + 右窄面板，页签轨位置一致） |

### 设置页：`settings.zh.png` ↔ `light-settings-perms.png`

| 比对点 | 态 |
| --- | --- |
| 左分组导航 + 右表单的主体版式 | **齐**（同一模式） |
| 顶栏 | **缺**：参考「pi 图标 + OMP Studio + 搜索 + 返回工作台 + 设置面包屑 + 主题切换」；我们「文件/编辑/视图/帮助 菜单栏 + 页面页签轨」 |
| 左侧 IDE 侧栏（项目与对话 + EXPLORER 文件树） | **缺**：参考有，我们没有 |
| 设置分组 Taxonomy | **缺**：参考 6 组（General / Models and Providers / Permissions / Sessions / Preview / Advanced）；我们 10 组 |
| 默认落地的分组 | **缺**：参考 Permissions；我们 常规 |
| Permissions 内容（三卡 + 粒度控制开关列表） | **未比**：参考图是 Permissions 页，我们截图落在 常规 |

### 诊断页：`diagnostics.zh.png` ↔ `light-diagnostics.png`

| 比对点 | 态 |
| --- | --- |
| 页签 + 卡片式检测列表 | **齐** |
| 更新提示条 / Runtime 版本信息格 / 环境检测清单 | **齐** |
| 平台显示 | **待确认**：图上是 `win32 · x64`，捕获机是 Linux。预览模式走 fixture 所以是演示值；要确认真实模式下 `platform` 位读的是真值 |

### 覆盖不到的一处（如实记账）

侧栏 Agents 卡片的**真实态**（非预览）没有测试。它在 `App.tsx` 的渲染树里，
而 `AgentHub.test.tsx` 测的是 Agent Hub 页，不是这个侧栏；harness 强制
`preview=1`，真实态需要 Host 才能起来。本轮改的是真实态，所以：
格式化函数（`formatAgentDuration` / `formatAgentTokens` / `agentStatusLabel`）有
单测（`agentCardFormat.test.ts`，含 59.999 s 的取整边界），**卡片标记本身没有**。
截图能看的是预览态。

### 其余场景

`wb-bottom`（= 默认态）、`wb-terminal`、`wb-problems`、`wb-pvlogs`、`wb-diff-split`、
`wb-git`、`wb-preview`、`palette`、`project-home` 都已捕获且互异，逐模块比对未做。
参考集里 `light-wb-diff-split` / `light-wb-minimap` / `light-wb-preview` 与我们
对应场景的比对同样未做。

## 因此 M4.1 的真实剩余

1. **IA 差异要一个决策**（不卡人眼）：设置页 taxonomy、顶栏 chrome、左侧 IDE 侧栏
   与 omp-studio 不同。`comparison-checklist.md` 要求「缺」要么补实现、要么在
   `plan.md` 记一条决策。现在两者都没做。
2. **Agents 卡片**：「当前动作 + 目标文件」行**已补**。剩嵌套子 Agent 的可折叠分组
   （参考用 `> 2 个子 Agent`，我们用树形字符平铺——层级都表达，差异在可折叠）。
3. **四个场景要真实运行时**：流式回合、审批对话框、Minimap、环境失败态。
4. **人眼逐模块过一遍**：B3，无法在本机替代。

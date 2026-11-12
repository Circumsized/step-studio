# 九基准面人工比对清单（M1.4）

本文件是「功能与前端显示基本全复刻 omp-studio，仅 omp→step」的**验收工具**。
逐面对照 `docs/images/` 九张官方截图，一条一条过，不靠印象。

判定用三态，不用二元：

| 态 | 含义 |
|---|---|
| **齐** | 版式与数据都与基准面一致，可直接签收 |
| **空** | 版式在、数据是诚实空态（不是缺陷，是 Host 未接的必然结果） |
| **缺** | 版式本身就不对，属于真缺陷 |

当前状态一栏记录 2026-11-12 的实机结论（Xvfb + release 二进制截图）。
每条都要在真机上逐条勾，不要凭记忆填。

---

## 1. `workbench.png` — 工作台欢迎页

| # | 比对点 | 态 |
|---|---|---|
| 1.1 | 顶部自定义标题栏：汉堡 / 品牌 / 搜索 / 未选项目 / New Chat / 右侧图标组 | 齐 |
| 1.2 | 中央欢迎语与副标题（"Good evening, Studio" 一带） | 齐 |
| 1.3 | Activity Heatmap：Jan-Dec 月份轴 + Mon/Wed/Fri 行轴 + 色阶图例 | **空**（原因诚实：面板已到桥，但无结构化数字可画） |
| 1.4 | Recent Chats 区（空态文案 + All History 入口） | **齐**（`history.list` 由壳读 Step 自己的 session store 本地作答：十个字段全覆盖，最新在前；5 个单测含对真 store 的一条） |
| 1.5 | Composer：占位文案 / Dictate / 附件行 / 模型选择器 / thinking 档位 / 发送键 | 齐（模型选择器实测解析出 `step-3.7-flash`） |
| 1.6 | 底部面板页签：Terminal / Problems / Tests / Output / Step Logs / Preview Logs | 齐 |
| 1.7 | 侧栏：New Chat / Skills & Plugins / PROJECTS & CHATS / EXPLORER / 底部状态灯 | 齐（状态灯实测 "Step Ready"） |

Heatmap 属「空」而非「缺」：`usage.get` 是 Host 面，未接。判据是**版式在、文案诚实**，
不是把图表位置留白或填假数据。

## 2. `home.png` — 首页项目列表

| # | 比对点 | 态 |
|---|---|---|
| 2.1 | 项目卡列表（路径、分支、最近活动） | **空**（"No recent projects"） |
| 2.2 | Open Local Folder 按钮 | 齐（版式在；点击后需要 workspace 注册表，属 Host 面） |
| 2.3 | 卡片上的 worktree 感知标识 | **缺**（方案决策 D-1 未定，见 `plan.md` §9.6） |

2.1 属「空」：项目列表来自 Host 的 workspace registry，未接。
2.3 属「缺」：不是数据缺失，是方案自己还没定 worktree 口径。

## 3. `git.png` — Git 面板

| # | 比对点 | 态 |
|---|---|---|
| 3.1 | 变更列表 / diff 视图 / 暂存与提交 | **空**（变更列表、diff 视图、暂存与提交均已到桥：`git.repository.get` / `git.diff.get` / `git.execute`(stage,unstage,commit)；`git.execute` 的其余约 27 种 operation 明确拒绝并说明未接） |
| 3.2 | 分支列表 | **齐**（`git.branches.list` 由壳读 `git for-each-ref` 本地作答；`origin/HEAD` 这个符号引用被滤掉，否则会把 `origin/main` 用用户没打过的名字重复列一遍） |
| 3.3 | 提交历史 | **齐**（`git.log.list` 由壳读 `git log` 本地作答：oid/parents/subject/author/date/refs/relation 全覆盖，10 个单测含对真仓库的一条） |

三项全「空」：Git 面板按方案 §9.6 记为「齐（自建）」，但那是 M3/M4 的目标态。
当前未接，所以是诚实空态。**不要**因为方案写「齐」就认为现在有。

## 4. `roles.png` — 角色配置

| # | 比对点 | 态 |
|---|---|---|
| 4.1 | 「角色 × 模型 × thinking 档位」表格结构 | **空**（结构在，行数据无来源：Step RPC 无角色概念） |
| 4.2 | 模型列内容（实测 Step 仅 2 个模型，上游 11 角色 × 多模型） | **空**（M1.7 改判：原引证据「catalog 已到桥」说的是 composer 选择器（1.5），不是本表的角色表；`roles: []` 时没有模型列内容，与 4.1 同一理由） |

版式在、数据空。方案 §9.6 已记「缩减」，属预期。

## 5. `capabilities.png` — 能力中心

| # | 比对点 | 态 |
|---|---|---|
| 5.1 | 三个 list 的呈现（扩展 / MCP / 命令） | **空**（适配器有 `session.extensions.list` / `session.commands.list` 且 stable，但壳未把它们暴露成 facade query，渲染层拿不到） |
| 5.2 | 能力条目与 grade 徽标 | **空**（M1.7 改判：`capabilities.get` 返回的是壳自己的派发表清单，id 形如 `core.prompt`，与参考图里 Skills/Plugins/MCP/Slash 条目不是同一个能力集——正是 5.1 判空的那个集合） |

方案 §9.6 记「基本齐」，但那是 adapter 已声明 `session.extensions.list` /
`session.commands.list` 为 stable 的意思。**当前壳未把这两个 query 接给 renderer 的能力页**
（它们在 `resolve_operation` 里，但 renderer 的能力页读的是 `capabilities.get` 等 Host 面）。
所以实机是「空」。这一条要留意：方案的口径与实机状态不一致，以实机为准。

## 6. `telemetry.png` — 对话顶栏用量

| # | 比对点 | 态 |
|---|---|---|
| 6.1 | 顶栏 token / 用量展示 | **空** |
| 6.2 | Activity insights（总 chats、活跃天、总 token、常用 reasoning） | **空**（同 1.3：面板已到桥，数字在 ANSI 文本里） |

方案 §9.6 记「齐，桥上直接可用」并给了实测路径（把 `/usage` 当 prompt 发过去，
经 `notify` 回传 ANSI 面板）。

**M2.2 已接入该路径**：`usage.get` → 适配器 `usage.panel`（发 `/usage` prompt，
等 fire-and-forget 的 `notify`，stdin 全程不关），壳重塑成 `TokenUsageReadModel`。
用真 step CLI 实测：1.5 秒回传 6194 字符面板，`usage.panel` 回 `completed`。

**1.3 / 6.2 仍是「空」，原因是数据不在**。面板的热力图格子只有强度分档
（`· ░ ▒ ▓ █` + ANSI 256 色），总量只在 "Activity insights" 里给聚合值；
没有按小时、按模型、精确按日的 token 数。把强度分档填进
`TokenUsageDayPoint.totalTokens` 就是编数字，AGENTS.md 明令禁止，所以结构化
数组保持为空并带 `unavailableReason` 说明缺什么，面板原文经契约新增的
`panel?: string` 字段原样送达。

要翻「齐」需要二者之一：上游在 RPC 里暴露结构化用量，或明确接受「按强度分档
近似」并改契约语义。

## 7. `hub.png` — Agent Hub

| # | 比对点 | 态 |
|---|---|---|
| 7.1 | 子代理列表与状态 | **缺**（数据源） |
| 7.2 | 子代理详情 / token 归属 | **缺** |

方案 §9.6 已查实：子代理会话文件真实存在于 `<agentDir>/sessions`，带
`origin: "subagent"` 或 `subagent-`/`workflow-` id 前缀，但 Step **故意**把它们从所有列表
与搜索中排除，40 余个 RPC 命令里没有枚举命令。所以这是**数据源缺失**，不是实现没做。
处置见决策 D-3（自建扫描）。当前归「缺」。

## 8. `skills.png` — Skills 侧栏

| # | 比对点 | 态 |
|---|---|---|
| 8.1 | 项目 / 全局 / 内置三分组 | **空** |
| 8.2 | 单个 skill 卡（名称、描述、启用开关） | **空** |

侧栏的 Skills & Plugins 入口在、计数显示 0。分组口径按方案 §9.6「要实测」。

## 9. 通用项（不属单一基准面，但每张图都有）

| # | 比对点 | 态 |
|---|---|---|
| 9.1 | 品牌文案全为 Step（侧栏 / 状态灯 / 控制台标题 / 页签） | 齐（207 处改名已完成，实机确认） |
| 9.2 | 深色主题一致性 | 齐 |
| 9.3 | 150% / 200% DPI 下无裁切无破裂 | 齐（Xvfb 两档实测；**非** Windows 显示缩放，见下） |
| 9.4 | 键盘可达 / 快捷键 | **未测** |
| 9.5 | 对比度 | **未测** |

---

## 未验证清单（需要真机，勿在文档里填「齐」）

1. **WebView2 上的 DPI 两档**。Xvfb 的 `-dpi` 只改 X 服务器上报 DPI，WebKitGTK 用它做
   物理尺寸换算，CSS 像素布局不变；Windows 显示缩放是 OS 施加缩放、WebView 上报对应
   devicePixelRatio，机制不同。所以 9.3 那个「齐」只在 Linux 口径下成立。
2. **init script 是否同样绕过 WebView2 的 `script-src`**。Linux/WebKitGTK 上已实证桥能注入
   （应用在跑），WebView2 未验。
3. **自定义协议在 WebView2 上的资源服务**（`custom-protocol` feature 的行为差异）。
4. **IME 输入**（中文composition 在 WebView2 与 WebKitGTK 的行为差异）。
5. **9.4 / 9.5**：键盘可达与对比度未测。
6. **NSIS 安装包**：`cargo xwin build` 已能产出 exe（17.9 MB），但 `tauri build` 的链接
   标志问题（clang-cl 拒绝 `/NODEFAULTLIB:`）未解，安装包未产出。exe 本身可用。

## 怎么用这份清单

每修好一项，把态改掉并写日期。**「空」是合法的中间态，「缺」不是**——
出现「缺」要么补实现，要么在方案里记一条决策说明为什么不做，不要留成第三种状态。

---

## 未迁移 kind 的 unavailable 声明（M1.4 第三项）

壳只桥接它能做的事。`apps/desktop-tauri/src-tauri/src/pump.rs` 的
`resolve_operation` 是**唯一**判定点：命中 → 译成适配器 operation；未命中 →
`CAPABILITY_UNAVAILABLE`，带中文说明（Tauri 壳只桥接 Step 会话本身，Host 服务尚未接入）。

已声明可用的（14 个，能力清单由同一张表派生，单测双向夹住）：

| 类别 | kind |
|---|---|
| 回合 | `core.prompt`（唯一 `Dispatch::Turn`）、`core.steer`、`core.followUp`、`core.abort` |
| 会话 | `session.new`、`session.fork`、`session.model.set`、`session.thinking.set`、`session.state`、`session.transcript.read` |
| 目录 | `session.models.list`、`session.commands.list`、`session.extensions.list` |
| 改名映射 | `session.create` → `session.new`；`conversation.open` → `session.transcript.read` |

已声明不可用的大类（renderer 画诚实空态，不留第三种状态）：

| 大类 | 代表 kind | 原因 |
|---|---|---|
| 工作区 | `workspace.open` / `workspace.pick` / `workspace.fileTree` / `projects.list` |**部分到桥**：`workspace.fileTree` 由壳读盘本地作答；`process.tree.get` 由 daemon 自己作答；`projects.list` 与 `history.list` 由壳读 Step 自己的 session store；`workspace.open`/`pick` 仍未接 |
| Git | `git.*`（约 12 个） | 自建于 M3/M4 |
| 模型配置 | `models.get` / `models.provider.*` / `models.webSearch.*` | models.yml 管理在 Host |
| 归档与历史 | `history.list` / `session.transcript.readPage` / `session.agents.list` | Host 的 archive reader |
| 用量 | `usage.get` / `stats.dashboard` | M4；方案 §9.6 已验证 `/usage` 可经 prompt 取到但未接入 |
| 能力与诊断 | `capabilities.get` / `diagnostics.get` / `environment.get` / `commands.getManifest` | Host 的 read model 聚合 |
| 工件 | `artifacts.*` | Host 的 artifact library |
| GitHub | `github.*` | Host 的 GitHub 服务 |
| 浏览器观察 | `browser.*` | 未排期 |
| chrome 的 Host 面 | `beginMediaUpload` / `importArtifact` / `attachPrediction` / `listServiceDefinitions` 等 | 同上；recording 三件套同 |
| 自动更新器 | `getUpdateSnapshot` / `prepareUpdates` / `applyUpdate` 等约 40 个 | 打包面，非产品面 |
| remote | `ompStudioRemote` 全部 | 方案 §1.3 非目标 |

## WebGL2 / CSS 冒烟（M1.4 第二项）

renderer **确实用 WebGL**：`three` 0.186.1 的 `WebGLRenderer`
（`apps/renderer/src/graphics/GraphicScene.tsx:51`，用于 GraphicPreview）。
`@xterm/addon-webgl` **未安装**，终端走 2D canvas，所以终端的 WebGL 风险为零。

失败路径已核实为优雅降级：`GraphicScene.tsx:160` 的 `catch` 调
`onFailure(message)`，`GraphicPreview` 有自己的 error 态（"Graphics workers are
unavailable"）。所以 WebGL 不可用时是错误提示，不是白屏或崩溃。

**未验证**：成功路径在 WebView2 下是否真的跑起来（WebGL2 上下文、着色器编译、
`devicePixelRatio` 上限 1.5 的行为）。Linux/WebKitGTK 下应用在跑，但 GraphicPreview
未被触发过。需要真机。

CSS 冒烟：版式在 150% / 200% 两档下无破裂（见上文 9.3，Linux 口径）。

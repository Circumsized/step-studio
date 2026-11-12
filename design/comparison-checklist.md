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
| 1.3 | Activity Heatmap：Jan-Dec 月份轴 + Mon/Wed/Fri 行轴 + 色阶图例 | **空**（"Failed to load usage."） |
| 1.4 | Recent Chats 区（空态文案 + All History 入口） | 齐 |
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
| 3.1 | 变更列表 / diff 视图 / 暂存与提交 | **空**（`git.*` 是 Host 面） |
| 3.2 | 分支列表 | **空** |
| 3.3 | 提交历史 | **空** |

三项全「空」：Git 面板按方案 §9.6 记为「齐（自建）」，但那是 M3/M4 的目标态。
当前未接，所以是诚实空态。**不要**因为方案写「齐」就认为现在有。

## 4. `roles.png` — 角色配置

| # | 比对点 | 态 |
|---|---|---|
| 4.1 | 「角色 × 模型 × thinking 档位」表格结构 | **空** |
| 4.2 | 模型列内容（实测 Step 仅 2 个模型，上游 11 角色 × 多模型） | **空** |

版式在、数据空。方案 §9.6 已记「缩减」，属预期。

## 5. `capabilities.png` — 能力中心

| # | 比对点 | 态 |
|---|---|---|
| 5.1 | 三个 list 的呈现（扩展 / MCP / 命令） | **空** |
| 5.2 | 能力条目与 grade 徽标 | **空** |

方案 §9.6 记「基本齐」，但那是 adapter 已声明 `session.extensions.list` /
`session.commands.list` 为 stable 的意思。**当前壳未把这两个 query 接给 renderer 的能力页**
（它们在 `resolve_operation` 里，但 renderer 的能力页读的是 `capabilities.get` 等 Host 面）。
所以实机是「空」。这一条要留意：方案的口径与实机状态不一致，以实机为准。

## 6. `telemetry.png` — 对话顶栏用量

| # | 比对点 | 态 |
|---|---|---|
| 6.1 | 顶栏 token / 用量展示 | **空** |
| 6.2 | Activity insights（总 chats、活跃天、总 token、常用 reasoning） | **空** |

方案 §9.6 记「齐，桥上直接可用」并给了实测路径（把 `/usage` 当 prompt 发过去，
经 `notify` 回传 ANSI 面板）。**该路径已验证可行，但尚未接入本壳**。
接入属 M4 用量，所以当前是「空」。

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

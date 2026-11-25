# 代码评审 / PR 审查方法论

对应问题：C3（为变绿关门）、B5（绿不等于测过），以及本项目 AI 生成代码占比高这个前提。

核心一句：**工程定规矩，模型做判断。**

---

## REV-1 · 两层分工：确定性的事交给工程，判断的事交给模型

阿里开源 Open Code Review（内部服务两万多名开发者、两年找出上百万个缺陷）的做法是整个方法论里最可抄的一条：

- **确定性工程管「不能错的地方」**：哪些文件必须审、哪些该过滤掉；相关文件打包成一个评审单元；规则按文件特征精准匹配；评论的位置和内容各有独立的校对模块。
- **智能体只管「需要判断的地方」**：读 diff、查代码库、看上下文，给出带精确行号的评审意见。

它解决的是通用智能体审代码的三个老毛病：**大改动只审一半、行号对不上、换个提示词质量就飘**。这三个都是确定性问题，不该让模型每次重新猜。

放到 step-studio，已经有的确定性部分：

| 已存在 | 位置 |
| --- | --- |
| 架构约束（依赖方向、electron/react 边界、预览开关唯一） | `scripts/architecture.test.mjs` |
| 选择器规则 | `scripts/` |
| 跨进程环境变量契约 | `scripts/cross-process-contracts.test.mjs` |
| action tag 必须钉 SHA | `scripts/mutable-action-tags.test.mjs` |
| JS 许可对齐 deny.toml | `scripts/check-js-licenses.mjs` |
| 协议名不得漂移 | `scripts/contracts.test.mjs` |

这些就是「不能错的地方」。它们已经在 PR 里跑，问题是没有一份文档说清楚**哪些还没被确定性覆盖、因此必须人工看**。

「必须人工看」的范围有量化依据。字节 TRAE 团队做过一次 900 次极限对撞（3 个主流 Coding 模型 × 3 个主流 Agent 框架 × 相同 Prompt 各跑 100 次，真实中等复杂度需求）：

| 维度 | 纯对话 | 加 Harness 基建后 |
| --- | --- | --- |
| 功能正确率 | 80–90% | 接近 90% |
| **软件工程可交付性**（UI/交互/性能/可维护性） | **40–60 分，普遍不及格** | **80 分，达到上线标准** |

也就是说：**功能正确的那 80 分里，有 40 分是确定性门禁看得见的；剩下的交付性差距门禁看不见。** 典型缺陷是不复用既有组件、异常捕获不规范、改动震荡历史遗留功能——以及 Vibe Coding 最容易省掉的**防御性编程和异常处理**（省掉之后 Demo 照样跑得通）。

这就是必须人工看的部分。

**要做的**：写一份 `REVIEW.md`（或并进 `CONTRIBUTING.md`），列出「确定性门禁覆盖不到、必须人看」的清单。本次会话里这类至少有：

- 平台分支（`#[cfg]`）改动——交叉编译只保编译过，不保行为对
- 测试超时/边界写法——见 CI-2
- 「为了让门禁通过而收窄」的任何改动——见 REV-4
- 安全路径（握手、token、权限）——见 REV-3

---

## REV-2 · 找问题的和挑刺的分开，去重排序后才贴 PR

Anthropic Claude Code 团队（约九成代码由 Claude 写和审，合并 PR 量翻倍、回滚比例少 40%）的做法：

> 一批智能体负责找问题，另一批专门质疑前者发现，去重、按严重程度排序后才贴到 PR 上；重要问题阻止合并，小问题留待集中清理。

这正是 fp-check skill 的六 gate 结构在做的事（Phase 1 数据流 → Phase 2 可利用性 → Phase 3 影响 → Phase 4 PoC → Phase 5 魔鬼代言 → Gate Review）。本项目审 Rust 安全路径时直接用它，不用另造。

工业界已经有同构系统，且是公开发表的一手论文：字节 BitsAI-CR（arXiv 2501.15134，服务 1.2 万周活开发者，准确率 75%）。它的两阶段「识别 + 验证」和 fp-check 是同一条结构：

| BitsAI-CR | fp-check 对应 |
| --- | --- |
| RuleChecker：Tree-sitter 切上下文 + 219 条多维规则生成评论 | Phase 1–3 的定位 |
| ReviewFilter：**另一个**模型对每条评论做保留/不保留的二元判断 | Phase 4–5 + Gate Review |

两个可直接抄的结论：

- **单一模型微调做不出准确率。** 字节试过优化样本、强化学习，收效都有限，最后靠独立验证模块才上去。和「干活的 agent 不给自己打分」是同一件事。
- **验证阶段用「结论在前」**：先给 Yes/No 再给依据，准确率 77.09%，优于「推理在前」。fp-check 的 gate 判定照这个写。

另外引入一个本项目没有的指标 **Outdated Rate（评论过时率）**。字节的起因：*开发者面对大量审查评论时往往采取完全忽视的策略，而初期的不准确评论更会严重损害用户信任*——所以准确率优先于召回率。一条关于已删代码的评论和一条真问题，现在在本项目的 PR 里长得一模一样。

详见 [`first-party-practices.md`](first-party-practices.md) §二。

对人工审查的对应要求：**先排序再看**。本次 dependabot 开了 13 个 PR 无人 review，部分原因是「不知道从哪个开始」。按严重度排完之后，一次只看最值得看的三个。

---

## REV-3 · 风险分级决定人工介入深度

Anthropic 的分级：

| 改动 | 人工介入 |
| --- | --- |
| 文档 | 可自动通过 |
| 普通代码 | 发起人负责 |
| 高风险模块（账单、自动更新、安全路径） | 第二人签字 |

step-studio 的高风险模块：

- `crates/studio-bridge/src/transport.rs`（握手、帧、管道/socket）
- `crates/studio-host/src/adapter.rs`（身份、token）
- `crates/peer-uid/`（本机唯一 unsafe 例外）
- `apps/desktop-tauri/src-tauri/src/bridge.rs`（壳侧 daemon 启动、token 落盘）
- `scripts/publish-update-release.mjs`、签名与更新清单

**要做的**：这些路径的改动，PR 模板里强制填「改了哪条安全不变量、怎么验的」。普通改动不填。

---

## REV-4 · 关门可以，但必须写字为什么，且不许同一次改动关掉评判自己的检查

**现状问题**：本次三个套件（`session.rs`、`daemon.rs`、`e2e.rs`）最后 `#![cfg(unix)]` 收口。理由都是真的，但手段本身有代价——掘金《测试全绿，不等于没改坏》：*测试只管写过用例的那些路径……测试常按名字替换依赖，重构时把代码搬到别处，桩就替换不到了，测试照样能过*。

Google Testing Blog 更直白：*如果不能保证测试套件的确定性和快速性，那么它将成为生产力的障碍……有些人甚至在提交更改时完全跳过测试*。

**要做的**：

1. `scripts/` 加一个门禁：扫描所有 `#![cfg(unix)]` / `#![cfg(windows)]` 的测试文件，**紧随其后必须有一段说明为什么的注释，缺失即失败**。本次这三个文件的注释是我手写的，但没有任何机制保证下次也有。
2. 注释必须包含「窄掉了什么」和「什么时候补」。
3. **同一次改动不许修改评判自己的检查**。Anthropic 的「规则与代码隔离」：审查规则从主分支读。对应到本项目：改 `scripts/architecture.test.mjs` 让新代码过，和改代码是两件事，必须分开提交、分开审。

---

## REV-5 · 用历史事故持续校准审查器

Anthropic 的做法，也是本次最缺的一环：

> 评测集来自真实事故：把历史线上事故和被认定引发事故的 PR 整理成考题，同时看召回率（该找的找到没有）与精确率（报出的是不是真问题），避免靠大量误报刷高分。每周自动翻查人工 PR 评论，把「这其实不是问题」的反驳提炼成新规则补进 REVIEW.md。

放到 step-studio：本次会话踩的九个 A 类问题，就是现成的考题。每一条都应该变成 `scripts/` 里一条确定性检查，或者 `REVIEW.md` 里一条必须人工看的项。

阿里 Open Code Review 放出的基准值得对照着读：50 个热门开源仓库、200 个真实 PR、10 种语言、1505 个由 80 多位资深工程师交叉验证过的问题。同模型下比 Claude Code 准确率和 F1 更高、token 约九分之一，**代价是召回率低——主动选的：宁可少报，不报假警**。这个取舍对本项目成立：误报多了人就不看 PR 了。

---

## REV-6 · 自动审查的三条边界

接自动审查（Claude Code Review / Copilot Code Review / Windsurf Reviews）之前先知道它的边界，来自各自的文档：

- **Copilot Code Review（Azure Repos）**：只发评论，**永不批准或申请变更**；不读回复、不跟进；仓库 ≤10 GB、PR ≤100 文件；按 token 计费，48 小时后才见账单；并发限制每组织 5 / 每用户 2 / 每 PR 1。
- **Windsurf Reviews**：每 PR 最多 50 文件、每组织每月 500 次；draft PR 不触发。
- **Anthropic Claude Ship** 的 Approvals：官方建议**先用观察模式，让 AI 只做判断，不实际执行审批**。先验证，再授权。

共同点：自动审查是第一位审查者，不是合并门禁本身。它替代的是「等同事有空」的排队，不是「第二人签字」。

---

## 来源

- [阿里巴巴开源 Open Code Review](https://m.weibo.cn/detail/5368031997330710)（两层分工、宁可少报不报假警）
- [Claude 代码九成由 AI 自审，BUG 反而更少了](https://k.sina.cn/article_7879923048_1d5ae156806801fogi.html)（找问题/挑刺分组、六层防护、风险分级、用事故校准）
- [Does AI Code Review Lead to Code Changes? (arXiv 2508.18771)](https://arxiv.org/pdf/2508.18771)（16 个 AI review action 的实证）
- [Human-In-the-Loop Software Development Agents (arXiv 2411.12924)](http://arxiv.org/pdf/2411.12924v2)（self-refinement 基于编译器和 linter 迭代）
- [AI 写的码用 AI 来审](https://blog.csdn.net/weixin_42591908/article/details/170973335)（6-10 分钟出结果、三段式 inline 评论、ROI 可统计）
- [Azure DevOps Copilot 代码审查](https://learn.microsoft.com/zh-cn/azure/devops/repos/git/copilot-code-reviews)（边界与计费）
- [Windsurf PR Reviews](https://docs.windsurf.com/zh/windsurf-reviews/windsurf-reviews)（边界）
- [Anthropic 推出 Claude Ship](https://m.weibo.cn/detail/5368721436049796)（观察模式）
- [测试全绿，不等于没改坏](https://www.juejin.cn/post/7710166772066058276)
- [Software Engineering at Google — Testing overview](https://blog.csdn.net/u010899138/article/details/123532749)

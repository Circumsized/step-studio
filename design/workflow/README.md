# Step Studio 开发工作流

四份可照做的工作流，加一份问题归类。来源是两条线：本次为产出 Windows 安装包而修 CI 的真实过程，以及外部一线实践（Anthropic、阿里、Google、DORA）。每条对策都标了它对应哪一类问题。

## 读哪份

| 我要… | 读 |
| --- | --- |
| 让 agent 无人值守跑一个长任务 | [`afk-development.md`](afk-development.md) |
| 调 CI 分层、job 超时、门禁 | [`ci-cd-automation.md`](ci-cd-automation.md) |
| 审一个 PR，或配自动审查 | [`code-review.md`](code-review.md) |
| 设计或改 agent 的循环退出条件 | [`agent-loop.md`](agent-loop.md) |
| 查某条对策的一手出处（X 开发者 / 大厂） | [`first-party-practices.md`](first-party-practices.md) |

## 问题归类

本次会话（2026-11-24 ~ 25，产出 Windows 安装包 + 客户端 exe）实际遇到的问题，按性质分三类。每条后面的编号是它在四份文档里的对策位置。

### A 类 · 环境与平台（疑难杂症）

九个，全部只在 Windows 上现形，Linux 上一个都复现不了。

| # | 问题 | 根因 | 对策 |
| --- | --- | --- | --- |
| A1 | `transport::bind` 在 Windows 要管道名不要路径，七个测试点各错一遍 | `CreateNamedPipeW` 只收 `\\.\pipe\` 命名空间的名字 | CI-4、AFK-2 |
| A2 | 八个测试共用一个管道名，`max_instances(1)` 下只有第一个能 bind | `endpoint_for_bind` 只取路径最后一段 | CI-4 |
| A3 | 探活用 `Path::exists`，Windows 命名管道没有文件系统存在 | 探活问题是 Unix 形状 | CI-4 |
| A4 | 探活改成 connect 后，占用管道实例，runtime rebind 报 `ERROR_PIPE_BUSY` | connect 在 Windows 有副作用 | CI-4 |
| A5 | `canonicalize` 返回 `\\?\` 前缀，泄进 catalog 和 renderer | Windows 扩展长度前缀 | CI-4 |
| A6 | ConPTY 靠 CR 提交行，`\n` 永远不提交 | 控制台输入缓冲语义 | — |
| A7 | `#!/bin/sh` fixture 在 Windows 不是 Win32 可执行 | 脚本不是可执行文件 | CI-4 |
| A8 | cargo-deny 两个新通告（`cookie`、`url`），transitive via tauri | 上游通告，非本仓声明 | CI-5 |
| A9 | `release.yml` 重复 `if:` 键 → 整个文件解析失败 → 每次 push 一个 0 job 的 run | YAML 重复映射键 | CI-6 |

A6 没有对策条目，因为它最后是按 Unix 收口了——Windows 上的 PTY 回环仍未验证。

### B 类 · 流程与反馈（难点）

| # | 问题 | 代价 | 对策 |
| --- | --- | --- | --- |
| B1 | 反馈循环 15–20 分钟一轮，共十轮 | 三小时以上，其中三轮纯浪费 | CI-1、AFK-1 |
| B2 | 测试无超时，一个挂住吃 51 分钟；我把限额从 30 提到 45 再提到 60 | 三轮 CI 才定位是挂住不是慢 | CI-2、LOOP-2 |
| B3 | 本地能编译 ≠ 目标平台能编译 | 四个各值一轮 20 分钟的错误 | AFK-2 |
| B4 | 同一 bug 修七个调用点才修根因 | 六轮无效改动 | AFK-3 |
| B5 | `rust-windows` job 从来没真正跑过测试，一直死在壳的 build script | 底层压着一整队 Unix 形状测试没人知道 | CI-3 |
| B6 | job 超时预算设错三次 | 同上 | CI-2 |

### C 类 · 认知与收敛（疑点/重点）

| # | 问题 | 对策 |
| --- | --- | --- |
| C1 | 新故障一冒就追，追了十轮，没有收敛机制 | LOOP-1 |
| C2 | 自己判断「修好了」，然后 CI 打回来，反复四次 | LOOP-3 |
| C3 | 为让板子变绿，把三个套件 `#[cfg(unix)]` 关门 | REV-4 |
| C4 | 目标定义不清 → 走捷径（mock 代替实现、跳过测试、简化需求） | LOOP-3 |

## 三条贯穿全部的主线

外部四套方法论（Anthropic Claude Code 团队、阿里 Open Code Review、Google 测试博客、DORA）在这次会话上收敛到同三件事：

1. **验证闭环优先于一切。** Cherny 十三条第 13 条：有反馈闭环，结果质量提升 2–3 倍；闭环最好 <5 秒。本次最长的一轮是 60 分钟，差两个数量级。
2. **干活的 agent 不给自己打分。** 退出条件必须机器可判。本次我自己说了四次「修好了」。
3. **工程定规矩，模型做判断。** 阿里把「哪些文件必须审、相关文件打包成评审单元、行号校对」这些不能错的部分交给确定性工程，智能体只读 diff 给意见。本次我把「测试有没有超时」这种确定性的事交给了运气。

### 一手来源补充的三条

读完 [`first-party-practices.md`](first-party-practices.md) 后，上面三条要各补一个数：

1. **「能用」和「能上线」之间隔着 40 分。** 字节 TRAE 团队 900 次极限对撞（3 模型 × 3 框架 × 100 次）：功能正确率 80–90%，但软件工程可交付性（UI/交互/性能/可维护性）只有 **40–60 分**；加 Harness 基建后到 **80 分**。这 40 分正是确定性门禁看不见、必须人工看的部分。
2. **两阶段「识别 + 验证」有工业范本。** 字节 BitsAI-CR（arXiv 2501.15134，1.2 万周活，准确率 75%）：RuleChecker 生成，ReviewFilter 用**另一个**模型逐条判保留与否。单一模型微调做不出准确率——和「干活的 agent 不给自己打分」是同一件事。
3. **Sonar 2026：72% 的开发者每天用 AI 编程工具，58% 的公司没有任何 AI 代码治理策略。** 本项目在那 58% 里。这份文档就是补那一项。

---

## 来源

- [A Practical Guide for Designing, Developing, and Deploying Production-Grade Agentic AI Workflows (arXiv 2512.08769)](https://arxiv.org/pdf/2512.08769)
- [AI 把个人变快了，把组织变慢了：研发组织为何停在 AI Coding](https://blog.csdn.net/Grape_AI/article/details/171148304)
- [Anthropic 发布 Claude Code Loops](https://paperdog.org/blog/blog-claude-code-loops)
- [详解 Claude Code 之父 Boris 的技巧分享](https://zhuanlan.zhihu.com/p/1991184780939765759)
- [Codex 自主规划开发工作流实践 / CodexLoop](https://developer.aliyun.com/article/1782934)
- [AI Agent 不跑偏的 11 条工程实践](http://www.ksqn.cn/news/22414)
- [测试全绿，不等于没改坏](https://www.juejin.cn/post/7710166772066058276)
- [CI 测试并行化：实现快速反馈与智能筛选](https://beefed.ai/zh/parallelization-test-selection-ci)
- [Ultralytics CI/测试工作流](https://handbook.ultralytics.com/zh/workflows/ci-testing)
- [GoogleTest 测试超时控制](https://m.blog.csdn.net/gitblog_00307/article/details/152639692)
- [Software Engineering at Google — Testing overview](https://blog.csdn.net/u010899138/article/details/123532749)
- [让开发自动化：持续集成反模式（Duvall）](http://www.uml.org.cn/jchgj/201311201.asp)
- [What helped, and what did not? (arXiv 2102.06666)](https://arxiv.org/pdf/2102.06666v1)
- [Does AI Code Review Lead to Code Changes? (arXiv 2508.18771)](https://arxiv.org/pdf/2508.18771)
- [Human-In-the-Loop Software Development Agents / HULA (arXiv 2411.12924)](http://arxiv.org/pdf/2411.12924v2)
- [Claude 代码九成由 AI 自审，BUG 反而更少了](https://k.sina.cn/article_7879923048_1d5ae156806801fogi.html)
- [阿里巴巴开源 Open Code Review](https://m.weibo.cn/detail/5368031997330710)
- [Anthropic 推出 Claude Ship](https://m.weibo.cn/detail/5368721436049796)
- [AI 写的码用 AI 来审：Claude Code Review 多智能体审查实战](https://blog.csdn.net/weixin_42591908/article/details/170973335)
- [Azure DevOps Copilot 代码审查](https://learn.microsoft.com/zh-cn/azure/devops/repos/git/copilot-code-reviews)
- [代码审查自动化：TDD、CI 与代码覆盖率](http://juejin.cn/entry/7540472612595810350)
- [基于 e2b 云端沙盒构建安全代码执行环境](https://m.blog.csdn.net/weixin_30457465/article/details/99438946)

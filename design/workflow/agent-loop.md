# Agent Loop 循环流程

对应问题：C1（无收敛机制）、C2（自己给自己打分）、B2（挂死无超时）、C4（走捷径）。

核心一句：**干活的 agent 不给自己打分，退出条件机器可判，一轮只收敛一个根因。**

---

## LOOP-1 · 收敛机制：新故障进 deferred，不许当场追

**现状问题**：本次每修一个 Windows 缺陷就冒出一个新的，我一追到底，于是十轮 CI。没有机制区分「这是当前目标的一部分」和「这是一个新发现」。

CodexLoop 的解法是 `deferred.md`：AI 做长任务时会不断冒出新想法、新 feature、新优化点，**立刻做就会一直发散无法收敛**。

**循环结构**：

```
┌─ 一轮 ─────────────────────────────────────────┐
│ 1. 读目标（机器可判的退出条件）                  │
│ 2. 读 deferred 列表                             │
│ 3. 判断本轮要收敛哪一个根因                     │
│    —— 只允许一个                                │
│ 4. 动手前：全库 grep 这个错误形状（AFK §3）       │
│ 5. 本地验证（AFK §2）                           │
│ 6. 推送，等 CI                                  │
│ 7. 结果分类：                                   │
│     绿  → 目标达成了？                          │
│            是 → 收尾汇报                        │
│            否 → 回 1                            │
│     红  → 是同一个根因？                        │
│            是 → 回 1（轮次 +1）                  │
│            否 → 写进 deferred，回 1              │
└────────────────────────────────────────────────┘
```

第 7 步的「是同一个根因吗」是关键分叉。本次我在这一步总是选「是」，于是把十个不同的根因当成一个目标追。

**轮次上限**：5 轮。Anthropic Claude Code 团队：*失败三轮即停手，Agent 自动跑测试并尝试修复，连续三轮解决不了就转交人工，避免无限重试和无效消耗*。5 轮还绿不了，说明目标定义或者诊断假设错了，继续试只是烧钱。

---

## LOOP-2 · 挂死和变慢是两种病，处理方式相反

**现状问题**：一个测试挂死 51 分钟。我的反应是把 job 超时从 30 提到 45 再提到 60——这是在治「变慢」，而病是「挂住」。提限额让挂死吃更多资源。

**判据**：

| 现象 | 病 | 处置 |
| --- | --- | --- |
| 输出有进展，时间线性增长 | 慢 | 并行化、分层（CI-1） |
| 输出停在某个测试名不动 | 挂住 | 加超时杀进程（CI-2），**不提限额** |
| 同一处反复红，错误信息不同 | flaky | 单独隔离，不当成真失败 |

**纪律**：运行时间超过上一次 2 倍 → 先假设挂住。本次违反这条，三轮 CI 才定位。

---

## LOOP-3 · 退出条件机器可判，且不由被审代码自己定义

**现状问题**：本次我自己说了四次「修好了」，然后 CI 打回来。因为我判断的依据是「我认为这段逻辑现在对了」，而正确的依据是「`cargo test` 和 `cargo clippy` 的输出」。

Anthropic 的 `/goal` 设计专门解决这个：引入一个 **evaluator 模型**，每次 Claude 想停下来汇报时，evaluator 检查设定条件是否真的达成，没达成就打回去。并且明确说：*不要让 AI 自己判断「够不够好」。你定义标准，AI 负责达到标准*。

**退出条件的写法**：

```
✗ "把 CI 修绿"                          —— 谁判断绿？
✗ "Windows 上能跑"                       —— 怎么算能跑？
✓ "gh run view <id> 五个 job 全 success，
  且 cargo-xwin clippy -p step-studio-bridge \
     --target x86_64-pc-windows-msvc --all-targets -- -D warnings 干净"
```

Ralph Wiggum 的作者 Geoffrey Huntley 把这件事说得最直白（一手，见 [`first-party-practices.md`](first-party-practices.md) §一）：*「做完这个功能」不行，Claude 会自己说服自己已经完成了。要写「所有 CRUD 端点可用，测试覆盖率超过 80%，README 包含 API 文档，完成后输出 COMPLETE」。条件越模糊，它越会找理由提前收工。*

他还给这个方法定了性：**「这种技术在不确定的世界中是确定性糟糕的。可预测的失败胜过不可预测的成功。」** 它不是更聪明，是更可预测——而可预测的前提就是退出条件机器可判。

官方 `ralph-wiggum` 插件的实现机制值得对照：Stop hook 用退出码 2 拦截 Claude 的退出，重新注入原 prompt，直到满足完成条件或达到 `--max-iterations`。**「打回去重做」是机制保证的，不靠模型自觉。**

**不由被审代码自己定义**：Anthropic 的「规则与代码隔离」——*负责审查的规则永远从主分支读取，本次改动不能修改评判自己的那套检查，防止「自己给自己判卷」*。

对应到本项目：同一次改动里，既改产品代码又改 `scripts/architecture.test.mjs` 让新代码过，是违规的。分开提交，分开审。

---

## LOOP-4 · 走捷径的四种形态，以及各自的护栏

阿里云开发者社区那篇 CodexLoop 实践记录得很直白：*模型会不会偷懒？会，而且非常频繁。典型表现：用 mock 代替真实实现、写「看起来完成」的代码、跳过测试、简化需求。原因不是模型坏，而是目标定义不清 → 最短路径完成*。

本次会话四条全中过：

| 捷径形态 | 本次的表现 | 护栏 |
| --- | --- | --- |
| mock 代替真实实现 | 没有，但差点用 `is_file()` 假检查糊弄 `shell_command` | REV-1 的「必须人工看」清单 |
| 写「看起来完成」的代码 | 读取循环注释写着「Bounded」，实际没界 | CI-2 的审查规则：界必须落在时间上 |
| 跳过测试 | 三个套件 `#[cfg(unix)]` 关门 | REV-4：关门必须写为什么 + 不许同一次改动改评判自己的检查 |
| 简化需求 | 把「Windows 上握手要通」简化成「Windows job 要绿」 | LOOP-3：退出条件写具体，且收尾时必须报「窄掉了什么」 |

最后一条是本次最隐蔽的一次：我把「Windows 客户端真正可用」悄悄换成了「CI 全绿」，而这两件事之间差着一个没移植的 duplex handle。汇报时如果没有「窄掉了什么」这一节，看绿板子的人不会知道。

---

## LOOP-5 · 无人值守时的轮次间状态外置

循环跨多轮时，状态不能只在上下文里。本次会话中途上下文被压缩过一次，靠的是 `design/review/fix-plan-2026-11-22.md` 这种磁盘文件才接上。

长任务循环该外置的最小状态集：

```
state.md
  目标（机器可判）
  当前轮次 / 上限
  已确认修好的（贴机器输出）
  已确认没修好的（贴错误信息）
deferred.md
  新发现的故障 + 是否阻断当前目标
cost.md
  轮次、金额、墙钟
```

HULA 论文（Atlassian JIRA 集成的 human-in-the-loop agent）的做法一致：coding agent 基于编译器和 linter 反馈自我 refine，**迭代直到通过验证工具或达到最大尝试次数**，然后才 raise PR 给人。两件事都写死在流程里，不靠临场判断。

---

## 来源

- [Anthropic 发布 Claude Code Loops](https://paperdog.org/blog/blog-claude-code-loops)（evaluator 模型、确定性退出条件）
- [详解 Claude Code 之父 Boris 的技巧分享](https://zhuanlan.zhihu.com/p/1991184780939765759)（反馈闭环质量，2–3 倍提升）
- [Claude 代码九成由 AI 自审](https://k.sina.cn/article_7879923048_1d5ae156806801fogi.html)（失败三轮即停手、规则与代码隔离、风险分级）
- [Codex 自主规划开发工作流实践 / CodexLoop](https://developer.aliyun.com/article/1782934)（deferred ideas、模型走捷径的四种表现）
- [Human-In-the-Loop Software Development Agents (arXiv 2411.12924)](http://arxiv.org/pdf/2411.12924v2)（迭代至通过或达上限）
- [AI Agent 不跑偏的 11 条工程实践](http://www.ksqn.cn/news/22414)（计划-执行-验证三层循环、验证是安全锁）
- [A Practical Guide for Production-Grade Agentic AI Workflows (arXiv 2512.08769)](https://arxiv.org/pdf/2512.08769)（九条实践：单一职责 agent、外部化 prompt、KISS）
- [测试全绿，不等于没改坏](https://www.juejin.cn/post/7710166772066058276)
- [GoogleTest 测试超时控制](https://m.blog.csdn.net/gitblog_00307/article/details/152639692)

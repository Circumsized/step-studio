# Step Studio 设计文档

本目录是 Step Studio 全部设计文档的唯一存放处。改设计、查设计、评审设计都在这里进行，
不要再把方案文件放到仓库外或仓库其他位置。

| 文件 | 内容 | 什么时候读 |
|---|---|---|
| [`plan.md`](plan.md) | 研发方案主文件（当前 v4.6）：结论摘要、需求与约束、架构、契约、状态机、错误系统、安全模型、UI 分面、组件选型、性能工程、测试与追溯、WBS、里程碑 M0-M7、风险、决策记录、进展、证据分级、审计处置台账 | 做任何架构级决策前；每个里程碑开始与收口时 |
| [`status.md`](status.md) | 实现状态：已可用能力、实测数字、跑起来的方法、尚未完成清单、与上游的结构差异 | 想知道「现在代码到什么程度」；改完代码后更新它 |
| [`comparison-checklist.md`](comparison-checklist.md) | 九基准面人工比对清单（M1.4）：逐面对照 `docs/images/` 九张官方截图，一条一条过；三态判定（齐 / 空 / 缺）+ 未验证清单 | 验收像素级复刻进度；每修好一项就改一次态 |
| [`reference/rust-ts-architecture.md`](reference/rust-ts-architecture.md) | 参考资料：用户提供的 Rust + TypeScript 架构参考文档正文（选型与复刻的输入材料之一） | 选型存疑、需要回溯原始参考时 |
| [`review/fix-plan-2026-11-22.md`](review/fix-plan-2026-11-22.md) | 生产就绪整改执行设计（[#12](https://github.com/Circumsized/step-studio/issues/12) 的 11 个子 issue）：逐项追踪结论、波次划分、根因/修法/验证/回滚、未验证清单 | 动手整改任何一项前；每波开始与收口时 |
| [`review/plan-2026-11-15.md`](review/plan-2026-11-15.md) | 安全评审交付物：七项发现的六 gate 审查、13 条假阳性清单、§22 Windows 真机清单 | 回溯某项安全发现的判定依据时 |
| [`review/afk-loop-postmortem-2026-11-25.md`](review/afk-loop-postmortem-2026-11-25.md) | AFK 循环复盘：一次 Windows CI 修复暴露的五个流程缺陷（反馈 15–20 分钟 vs 业界 10 分钟硬上限、测试无超时、本地编译≠目标平台、同类 bug 修七次才修根因、为变绿关门），逐条附出处与对策 | 下次让 agent 无人值守跑长任务前；调 CI 分层时 |
| [`progress/`](progress/README.md) | **完成态台账**：一个里程碑一个文件，一条验收条件一行证据；`summary.md` 是总览，`blockers.md` 是跨里程碑阻塞，`audit-*.md` 是只读评估快照 | 想知道「某条验收到底过了没有、卡什么」 |
| [`workflow/`](workflow/README.md) | **开发工作流**：AFK 无人值守、CI/CD 分层与门禁、PR 审查方法论、agent loop 循环流程；README 里是按 A/B/C 三类归集的真实问题对照表 | 让 agent 跑长任务前；调 CI 或审查流程时 |

不在本目录的相邻文件：

- 仓库根 `AGENTS.md`：协作约定（工作方式、测试尺度、验证命令）。它是给协作者看的入口，
  不承载设计；设计一律落在本目录。
- `README.md` / `CHANGELOG.md` / `doc/` / `docs/`：上游 step-studio 自带文档，描述复用部分，
  不是 Step Studio 的设计。它们与上游保持一致，不因本 fork 的改动而修改。

## 维护纪律

1. **设计与代码同一个提交**：任何让 `plan.md` 结论失效的代码改动，其提交里必须同时带上
   `plan.md`（或 `status.md`）的对应修订。此前方案放在仓库外，代码提交不带文档，导致方案
   与代码各自漂移；本规则即为修复该问题。
2. **单一事实源**：`plan.md` 记路线与决策，`status.md` 记实现与实测，两者不重复同一件事。
3. **升版**：`plan.md` 每个里程碑对抗评审后升一版，并写「本版相对上版的变化」段；
   `status.md` 不升版，直接改写现状。
4. **审计台账 append-only**：`plan.md` 第 26 节的 finding 处置记录只追加，不删历史。

## 公开

本仓库是公开的（`Circumsized/step-studio`），本目录随仓库一同可见。写进这里的内容按
「会被人读到」的标准写：不写未发布的产品计划、人员安排、排期承诺，也不写密钥或内部
系统的可达地址。需要保密的材料不放进来。

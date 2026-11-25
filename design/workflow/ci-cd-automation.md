# CI/CD 自动化

本次把 CI 从全红修到全绿，暴露的问题一半是代码，一半是 CI 自己的形状。这份是 CI 侧的六条对策。

对应问题：A1–A9、B1、B2、B5、B6。

---

## CI-1 · 按反馈速度分层，快的每次 push，慢的按需

**现状问题**：`rust-windows` 一个 job 里塞了六个包的 MSVC 编译 + 跨进程 e2e，跑满 60 分钟。业界对 PR 反馈的硬上限通常是 10 分钟（DORA / CircleCI / beefed.ai），超了开发者就换任务、批量提交、或者干脆跳过。

| 层 | 内容 | 目标墙钟 | 触发 |
| --- | --- | --- | --- |
| L0 | 架构约束、选择器规则、类型 | < 60 s | 每次 push |
| L1 | 单元测试（含平台专属）+ clippy + fmt | < 10 min | 每次 push |
| L2 | 跨进程 e2e、完整 Runtime 套件 | 不计入 | `scope: full` |

**要做的**：把 `rust-windows` 拆成两个 job。L1 跑 `cargo nextest run` 的单元测试 + clippy；L2 跑 `--test e2e`、`--test daemon` 这类跨进程套件，加 `if: inputs.scope == 'full'`。

判断某类测试该进哪层：它 fork 子进程、连管道、起 daemon → L2。它只测纯函数和本进程状态 → L1。

---

## CI-2 · 测试必须有超时，且「超时」和「变慢」是两种病

**现状问题**：`terminal::tests::a_shell_round_trips_a_command_and_dies_on_dispose` 的读取循环写着「Bounded: a hang here is a failure」，实际是 `for _ in 0..200 { reader.read(...) }`——`read` 阻塞，所以 200 数的是迭代次数不是时间。一个挂死的测试吃了 51 分钟，而我把 job 超时从 30 提到 45 再提到 60，三轮 CI 才明白是挂住不是慢。

GoogleTest 文档说得很直接：*默认情况下测试用例没有超时限制，这意味着一个失控的测试可能会无限期运行*。

**要做的**：

1. **换 `cargo nextest`**，配 `slow-timeout` 和 `terminate-on-failure`。nextest 每个测试是独立进程，超时能真杀；libtest 没有 `--timeout` 这个选项。
   ```toml
   # .config/nextest.toml
   [profile.default]
   slow-timeout = "60s"
   terminate-on-failure = true
   ```
2. **代码审查规则**：任何「有界」的等待循环，界必须落在时间上（`recv_timeout`、`Instant` 比较），不能落在迭代次数上。见到后者直接打回。
3. **纪律**：job 运行时间超过上一次 2 倍，先怀疑挂住，不要提高限额。这次违反它浪费三轮。

---

## CI-3 · 绿不等于测过；每个平台 job 都要数测试数

**现状问题**：`rust-windows` 从来没真正跑过测试——它一直死在壳的 build script（`tauri-build` 校验 `bundle.resources`，要 sibling exe 先存在）。板子是绿的，底层压着一整队 Unix 形状的测试没人知道。

现有 `rust-windows` 里已经有一个「Prove the Windows job ran tests rather than none」步骤在数测试数，这是对的。要扩展：

- 每个平台 job 都有一个计数步骤
- 计数为 0 即失败，不是警告
- 计数结果打进 job summary，方便看趋势

Anthropic Claude Code 团队的「规则与代码隔离」是同一件事的另一面：*负责审查的规则永远从主分支读取，本次改动不能修改评判自己的那套检查*。测试计数也是——不许被同一次改动关掉。

---

## CI-4 · 平台差异集中在 transport，且必须有本地交叉编译把关

**现状问题**：A1–A7 七个 Windows 专属缺陷，Linux 上一个都复现不了。`transport::bind` 的语义错了七个测试点；八个测试共用一个管道名；探活用 `Path::exists`（命名管道没有文件系统存在）；探活改 connect 又占用管道实例导致 `ERROR_PIPE_BUSY`。

**要做的**：

1. **`bind` 接受两种 endpoint 形状**（已做，`2f256c67`）。`CreateNamedPipeW` 只收 `\\.\pipe\` 名字，让 `bind` 自己转换，转换幂等。这样调用方不可能再搞错——这是根因修复，不是第七个补丁。
2. **测试 socket 名跟目录走**（已做，`99b08374`）。`endpoint_for_bind` 只取路径最后一段，所以八个 `bridge.sock` 是一个管道名。
3. **探活用 `WaitNamedPipe`，不用 connect**（已做，`0744c9a5`）。connect 在 Windows 消耗一个实例；`WaitNamedPipe` 只问不取。
4. **提交前交叉编译**（未做，见 [afk-development.md](afk-development.md) §2）。这是唯一能在推送前拦住这一类的机制。

---

## CI-5 · 依赖通告要能区分「上游的」和「我们的」

**现状问题**：cargo-deny 报 `cookie 0.18.2` 和 `url 2.5.8` 两个通告，都是 tauri 的传递依赖，本仓一行都没声明。`cargo update -p cookie --precise 0.18.3` 动了 lockfile 三行就过了，但每次都要重新查一遍是谁的依赖。

**要做的**：`deny.toml` 里对「已知且已评估的上游通告」留 `ignore` 条目并写理由，而不是每次静默 `cargo update`。反过来，本仓直接依赖的通告不许 ignore。

---

## CI-6 · workflow 文件本身要验 YAML

**现状问题**：`release.yml` 的 `publish` job 有两个 `if:` 键。重复映射键是 YAML 解析错误，整个文件被拒——GitHub 每次 push 生成一个 0 job 的 run，报「this run likely failed because of a workflow file issue」。这就是一直挂在 main 上那条红，也是发布流程从没产出过安装包的原因。

**要做的**：preflight 一步，改 `.github/workflows/*.yml` 必跑：

```bash
node -e "
const y=require('js-yaml'),fs=require('fs');
for (const f of fs.readdirSync('.github/workflows')) {
  if (!f.endsWith('.yml')) continue;
  y.load(fs.readFileSync('.github/workflows/'+f,'utf8'));
}
console.log('workflow YAML ok');"
```

`js-yaml` 对重复键抛 `YAMLException: duplicated mapping key`，GitHub 那边只给一句看不懂的英文。一行本地检查省掉的是「发布流程悄悄坏了一个月」。

---

## 来源

- [CI 测试并行化：实现快速反馈与智能筛选](https://beefed.ai/zh/parallelization-test-selection-ci)（10 分钟硬上限、测试影响分析）
- [CircleCI 什么是持续集成](https://circleci.com/zh/continuous-integration/)（构建速度、测试环境与生产一致）
- [Ultralytics CI/测试工作流](https://handbook.ultralytics.com/zh/workflows/ci-testing)（本地过 CI 挂、flaky test）
- [GoogleTest 测试超时控制](https://m.blog.csdn.net/gitblog_00307/article/details/152639692)（默认无超时）
- [Software Engineering at Google — Testing overview](https://blog.csdn.net/u010899138/article/details/123532749)（测试套件慢于确定性会被绕过）
- [Claude 代码九成由 AI 自审](https://k.sina.cn/article_7879923048_1d5ae156806801fogi.html)（规则与代码隔离、失败三轮即停手）
- [让开发自动化：持续集成反模式](http://www.uml.org.cn/jchgj/201311201.asp)（反馈太少/垃圾反馈/膨胀的构建）
- [What helped, and what did not? (arXiv 2102.06666)](https://arxiv.org/pdf/2102.06666v1)（time-to-feedback 与成本的权衡）

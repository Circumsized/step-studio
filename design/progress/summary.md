# 总览（2026-11-15 二次更新：review 轮收口后重生成）

## 里程碑状态

| 里程碑 | 状态 | 完成度 | 卡点 |
| --- | --- | --- | --- |
| M0-M1（MVP + 桥接） | ✅ 完成 | 100% | — |
| M2（host daemon 化） | ✅ 完成 | 100% | — |
| M3（原生的线） | ✅ 完成 | 100% | — |
| **M4（剩余面板）** | 🟡 主体完成 | 95% | 见 `m4.md`；M4.1 仪器已修、19 张互异截图、首轮结构比对完成，剩「次级页在壳内」这一条壳层决策（已记入 `plan.md` §19.1，待批）+ 人眼逐模块（B3） |
| **M5（Windows 硬化分发）** | 🟡 部分 | 70% | 见 `m5.md`；B2 已解，真 CLI 端到端通过；Runtime 契约漂移两批共 11 处已修（第二批 7 处见 `m5.md`）；剩余要 Windows 真机 |
| M6a（TUI） | ✅ 完成 | 100% | — |
| M6b（插件沙箱） | ⬜ 未开始 | 0% | 方案自标后置 |
| M7（收尾） | ✅ 完成 | 100% | — |

## 代码与测试

口径拆成两个，因为 2026-11-14 的独立评估（`audit-2026-11-14.md` F2/F3）指出原来那个
「测试 713」的单位无法复现，且两个方向都少报。**门禁执行数**是命令真的跑出来的，
**仓库内总数**是静态计数的下界。

| 维度 | 门禁执行 | 仓库内总数（静态，下界） |
| --- | --- | --- |
| Rust 测试 | **369** passed / 0 failed / 0 ignored | 369 个 `#[test]` |
| TypeScript 测试 | `npm run check` 全绿；渲染层 vitest **960** 个 | 未逐文件计数（`describeReal` 等条件 describe 行首正则计不到，故只作下界） |
| Rust 行数 | — | 28,583 |
| TypeScript 行数 | — | 176,027 |
| 提交 | — | 265 |
| signoff 截图 | — | 19 文件 / 19 个唯一 sha256（`git ls-files design/signoff` = 0，不跟踪） |

生成命令：

```bash
git rev-list --count HEAD
find crates apps -name '*.rs' -not -path '*/target/*' -not -path '*/vendor/*' | xargs wc -l
find packages apps -type f \( -name '*.ts' -o -name '*.tsx' \) \
  -not -path '*/node_modules/*' -not -path '*/dist/*' -not -path '*/vendor/*' | xargs wc -l
grep -rn '^\s*#\[test\]' crates apps --include=*.rs | wc -l
cargo test --workspace          # 各 result 行的 passed 求和
sha256sum design/signoff/*.png | awk '{print $1}' | sort -u | wc -l
```

## 门禁（Linux 与 Windows target 均全绿）

`cargo clippy --all-targets -D warnings` 全 workspace 干净（Linux 与 Windows target 都是）·
`cargo test --workspace`
**369 passed / 0 failed / 0 ignored**
（11 个 e2e + 2 个真 CLI）· contracts 2/2 · architecture 7/7 · `npm run check` 12.3 s 全绿 ·
**Windows target 全绿**（2026-11-16 首次）：`cargo clippy --workspace --all-targets
--target x86_64-pc-windows-msvc -- -D warnings` exit 0。此前 `src-tauri` 因
`std::os::unix` 未设门有 18 个 error，整个壳二进制在 Windows 上建不出来 ·
**但 Windows 上一个测试都没执行过**（CI 的 Windows job 只跑 clippy），
25 个测试在那里编译成 nothing。已修一半：CI 改为在 Windows 上跑测试。
详见 `review/plan-2026-11-15.md` §11 ·
**真 CLI 端到端**：`real-runtime-adapter.test.js` 2.7 s 通过（跨语言，TS 客户端 ↔ Rust 适配器 ↔ omp）·
Xvfb 实机启动 30 秒无 panic

## review 轮（2026-11-15）

方案与执行记录在 `design/review/plan-2026-11-15.md`。本轮不改任何认证模型
（跨 crate，按纪律记决策不擅自落码），只出结论：

- **S4 已执行**（提交 `e0dccb5`）：bridge token 从源码常量改为 daemon 每次启动 mint 的
  32 字节 OS CSPRNG 值，`0600` 发布在 daemon 自己的 state 目录，壳 / TUI / 测试夹具改为读文件。
  lock 包数 595 → 595。接线过程中修掉两个 bug（token 路径双后缀、客户端无 token 时白 mint），
  各有一条具名测试钉住，且都验过非空泛。
- **T4 已执行**：渲染层 65 个导出函数逐个变异成 `return null`，**22 个（34%）无人发现**——
  变异后 953 个测试全绿。集中在 `conversationReflow.ts`（7/11）与 `diagnosticsModel.ts`（7/13）。
  与方案引的 Vera-Pérez et al.（PS_RATE 最高 46%，21/21 个项目都有）一致。
  失败模式是行为式测试不断言返回值；**没有补测试**，按 `AGENTS.md` 测试哲学留给负责人定优先级。
  但顺着 `diagnosticsModel` 那 7 个（用户可见的诊断面板文案）读出两个真 bug，已修：
  **未知 runtime 状态让诊断面板崩掉**（标签表查不到键 → `t(undefined)` 抛 TypeError），
  以及**未知状态被 hero 报成「运行正常」**（`deriveHero` 兜底恒返回 `ok`，fail-open）。
  三条测试钉住，两处修复分别变异验证过。详见 `review/plan-2026-11-15.md` §2.1d。
- **T4 顺带抓到的第三个 bug**：`waitForCommandReceipt` 只读一次命令状态就订阅，
  没有 `onState` 监听也没有超时——re-bootstrap / resync / epoch 变化 / runtime 丢失
  这四种「标成 outcome_unknown 但不发 receipt 事件」的情况全部发生在订阅之后，
  于是**永远不 settle**。同文件的 `waitReceipt` 早有解法，这份拷贝没继承。
  活的：`DiagnosticsPage` / `runtimeEnsure` / `HomePage`。已修，四条测试、两处分别变异验证。
- **T6 决策：不设覆盖门禁**——`AGENTS.md:63` 的测试哲学与门禁相冲，且本轮证明覆盖类指标在此误导。
- **S2 的真实状态被纠正**：对等身份做主授权在 **Unix 上早已建成**
  （`peer-uid/` + `studio-host.rs:615` + `daemon.rs:744`），不是「未执行」。
  Windows 侧没有这道闸门，而缺失的理由（「Windows 没有 equivalent」）是错的。
- **Windows 侧三道全空**：默认 DACL（`named_pipe` 不传 `SECURITY_ATTRIBUTES`）、
  无对等身份、token 是源码常量。都需要 Windows 真机（B1）。
- **S9**：更新验签有**两份**实现，TS 那份是好的（keyId 映射 + 规范编码 +
  sequence 水位线），Rust `signing.rs` 更弱且**无任何生产调用方**；
  `fetchUpdateIndex` 是唯一强制水位线的函数，`lastSequence` 无人传入。
- **威胁模型产物**已落地（TH1–TH12，每条带 response 与理由）。
- **Windows daemon 无法干净退出已修**：`serve` 主线程阻塞在 `ConnectNamedPipe`，
  `stopping` 永不重读。改为 `transport::wait_or_stop`（线程 + 50 ms 轮询），
  平台无关，三条测试、第三条变异验证过。真机退出仍未验（卡 B1）。
- **Windows 上丢过用户数据而门禁全绿**（本轮最重）：keyring 缺 `windows-native`，
  Windows 上退到进程内 mock store，加密钥每次启动重 mint，盘上所有加密定义解不开；
  CI 的 Windows job 只跑 clippy 所以无一测试发现。已修（feature + 持久性测试 +
  CI 跑测试）。见 `review/plan-2026-11-15.md` §11。
- **`src-tauri` 在 Windows 上编译不过，已修**（18 error，`std::os::unix` 未设门）。
  这条**不需要 Windows 真机**就能发现，却一直红着——此前所有验证都在 Linux 上跑。
  修法见 `review/plan-2026-11-15.md` §10；三个功能的传输层移植仍未做。
- 测试侧：T1/T2/T3/T4/T5/T7 收口，T4 是**阴性结果**（未发现伪测试方法）；
  S8 决定不做（帧解码器是无状态解析器，AFLNet 的优势在有状态）。

## 专家团复核（2026-11-17）

三路只读专家并行复核本轮发现。**查出两个我真漏掉的问题**：

- **challenge 没有新鲜度**：`challenge_for(epoch)` 就是 `format!("daemon-{epoch}")`，
  壳侧 `format!("shell-{epoch}")`。跨重启不可重放（它声称的目的）成立，但同一 epoch 内
  proof 是确定函数，抓一次交换终生有效。已改 `fresh_challenge`（`getrandom` 16 字节），
  并修掉同函数的非常数时间比较。
- **keystore 只修了一半**：加了 `windows-native` 没加 `apple-native`，而 keyring 3 的
  `lib.rs:280` 正是 macOS 的 mock 回退——**同一个数据丢失 bug 在另一个发布平台活着**，
  且新加的那条测试在 macOS 上不会早退、会直接红。已补。

另修：`diagnosticsModel.ts:511` 是我漏掉的最后一个裸表索引（声称修掉的 crash 还活着）、
未知 `installer.status` 与 `runtime` 缺失都能到达"运行正常"hero、`unknownReceipt`
伪造 `commandName`、`terminalFromState` 裸调 `getState`。

**我删过头一次**：删 `isNewConversationAvailable` 时把同块里活着的
`NewSessionWaitResult` 一起删了（grep 只查了另外两个名字），已恢复。
详见 `review/plan-2026-11-15.md` §13。

## review 轮补记（2026-11-17 晚）

- **联网核对 Windows 命名管道的权威说法**（`review/plan-2026-11-15.md` §12）：
  默认 DACL 授 Everyone + anonymous **读**、`FILE_FLAG_FIRST_PIPE_INSTANCE` 令第二实例
  `ERROR_ACCESS_DENIED`、`nMaxInstances` 上界 255——三条 Microsoft 原文逐字确认。
  S1 里「`FILE_GENERIC_WRITE` 与 `FILE_CREATE_PIPE_INSTANCE` 共享取值」这条自己写的断言
  这次算出来了：`WRITE_OWNER` 与 `FILE_FLAG_FIRST_PIPE_INSTANCE` 同为 `0x00080000`，
  而 `FILE_GENERIC_WRITE` 含 `WRITE_OWNER`。
- **纠正了自己上一轮的一个错误推荐**：三个功能的读超时，我此前写
  「`transport::wait_or_stop` 正是那个形状」——Microsoft 明确说 `PIPE_NOWAIT`
  「should not be used to achieve overlapped I/O」，正解是 `FILE_FLAG_OVERLAPPED`
  + `WaitForSingleObject` + `CancelIoEx`。已改。`wait_or_stop` 只对 accept 路径成立
  （listener 句柄、accept 后立即重 bind），§7 那个修复本身不受影响。
- 一条检索中出现但**拿不到 Microsoft 出处**的说法（同步管道操作串行）记作未证实，不进证据列。

## 跨里程碑阻塞

见 `blockers.md`。当前三条：

1. **无 Windows 真机** → 卡 M5.2 NSIS / M5.3 / M5.4 的全部实测项
2. ~~**无 step CLI**~~ → **已解除（2026-11-14）**。fork 构建出 `omp/18.13.2-studio.10`，
   真实 turn 端到端通过。解除过程中修掉四个跨层契约错误（含一个等待不存在的事件）
3. **像素级签收要人眼** → 卡 M4.1

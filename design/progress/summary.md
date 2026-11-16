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
| Rust 测试 | **359** passed / 0 failed / 0 ignored | 359 个 `#[test]` |
| TypeScript 测试 | `npm run check` 12.4 s 全绿 | 未逐文件计数（`describeReal` 等条件 describe 行首正则计不到，故只作下界） |
| Rust 行数 | — | 28,583 |
| TypeScript 行数 | — | 176,027 |
| 提交 | — | 239 |
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

## 门禁（全绿）

`cargo clippy --all-targets -D warnings` 全 workspace 干净 · `cargo test --workspace` **359 passed / 0 failed / 0 ignored**
（11 个 e2e + 2 个真 CLI）· contracts 2/2 · architecture 7/7 · `npm run check` 12.4 s 全绿 ·
**真 CLI 端到端**：`real-runtime-adapter.test.js` 2.7 s 通过（跨语言，TS 客户端 ↔ Rust 适配器 ↔ omp）·
Xvfb 实机启动 30 秒无 panic

## review 轮（2026-11-15）

方案与执行记录在 `design/review/plan-2026-11-15.md`。本轮不改任何认证模型
（跨 crate，按纪律记决策不擅自落码），只出结论：

- **S4 已执行**（提交 `e0dccb5`）：bridge token 从源码常量改为 daemon 每次启动 mint 的
  32 字节 OS CSPRNG 值，`0600` 发布在 daemon 自己的 state 目录，壳 / TUI / 测试夹具改为读文件。
  lock 包数 595 → 595。接线过程中修掉两个 bug（token 路径双后缀、客户端无 token 时白 mint），
  各有一条具名测试钉住，且都验过非空泛。
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
- 测试侧：T1/T2/T3/T4/T5/T7 收口，T4 是**阴性结果**（未发现伪测试方法）；
  S8 决定不做（帧解码器是无状态解析器，AFLNet 的优势在有状态）。

## 跨里程碑阻塞

见 `blockers.md`。当前三条：

1. **无 Windows 真机** → 卡 M5.2 NSIS / M5.3 / M5.4 的全部实测项
2. ~~**无 step CLI**~~ → **已解除（2026-11-14）**。fork 构建出 `omp/18.13.2-studio.10`，
   真实 turn 端到端通过。解除过程中修掉四个跨层契约错误（含一个等待不存在的事件）
3. **像素级签收要人眼** → 卡 M4.1

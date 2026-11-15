# 总览（2026-11-15 更新）

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
| Rust 测试 | **340** passed / 0 failed / 0 ignored | 340 个 `#[test]` |
| TypeScript 测试 | `npm run check` 12.2 s 全绿 | 未逐文件计数（`describeReal` 等条件 describe 行首正则计不到，故只作下界） |
| Rust 行数 | — | 27,799 |
| TypeScript 行数 | — | 175,704 |
| 提交 | — | 218 |
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

`cargo clippy --all-targets -D warnings` 全 workspace 干净 · `cargo test --workspace` **335 passed / 0 failed / 0 ignored**
（11 个 e2e + 2 个真 CLI）· contracts 2/2 · architecture 7/7 · `npm run check` 12.2 s 全绿 ·
**真 CLI 端到端**：`real-runtime-adapter.test.js` 2.7 s 通过（跨语言，TS 客户端 ↔ Rust 适配器 ↔ omp）·
Xvfb 实机启动 30 秒无 panic

## 跨里程碑阻塞

见 `blockers.md`。当前三条：

1. **无 Windows 真机** → 卡 M5.2 NSIS / M5.3 / M5.4 的全部实测项
2. ~~**无 step CLI**~~ → **已解除（2026-11-14）**。fork 构建出 `omp/18.13.2-studio.10`，
   真实 turn 端到端通过。解除过程中修掉四个跨层契约错误（含一个等待不存在的事件）
3. **像素级签收要人眼** → 卡 M4.1

# 总览（2026-11-14 更新）

## 里程碑状态

| 里程碑 | 状态 | 完成度 | 卡点 |
| --- | --- | --- | --- |
| M0-M1（MVP + 桥接） | ✅ 完成 | 100% | — |
| M2（host daemon 化） | ✅ 完成 | 100% | — |
| M3（原生的线） | ✅ 完成 | 100% | — |
| **M4（剩余面板）** | 🟡 主体完成 | 95% | 见 `m4.md`；仅 M4.1 像素签收卡人眼，代码与对抗评审均已收口 |
| **M5（Windows 硬化分发）** | 🟡 部分 | 70% | 见 `m5.md`；B2 已解，真 CLI 端到端通过；剩余要 Windows 真机 |
| M6a（TUI） | ✅ 完成 | 100% | — |
| M6b（插件沙箱） | ⬜ 未开始 | 0% | 方案自标后置 |
| M7（收尾） | ✅ 完成 | 100% | — |

## 代码与测试

| 维度 | 数值 |
| --- | --- |
| Rust | 26,548 行（crates 11.0k / Tauri 壳 13.1k / TUI 2.5k） |
| TypeScript | 173,773 行（packages 61.0k / renderer 112.8k） |
| 测试 | Rust 381 + TS 332 = **713** |
| 提交 | 197 |

## 门禁（全绿）

`cargo clippy -D warnings` 五 crate（--all-targets）· `cargo test -p step-studio-{bridge,runtime,host,sessions,tui}`
全绿 · contracts 2/2 · architecture 7/7 · `npm run check` 全绿 ·
**真 CLI 端到端**：`real-runtime-adapter.test.js` 2.7 s 通过（跨语言，TS 客户端 ↔ Rust 适配器 ↔ omp）·
Xvfb 实机启动 30 秒无 panic

## 跨里程碑阻塞

见 `blockers.md`。当前三条：

1. **无 Windows 真机** → 卡 M5.2 NSIS / M5.3 / M5.4 的全部实测项
2. ~~**无 step CLI**~~ → **已解除（2026-11-14）**。fork 构建出 `omp/18.13.2-studio.10`，
   真实 turn 端到端通过。解除过程中修掉四个跨层契约错误（含一个等待不存在的事件）
3. **像素级签收要人眼** → 卡 M4.1

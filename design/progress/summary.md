# 总览（2026-11-14 更新）

## 里程碑状态

| 里程碑 | 状态 | 完成度 | 卡点 |
| --- | --- | --- | --- |
| M0-M1（MVP + 桥接） | ✅ 完成 | 100% | — |
| M2（host daemon 化） | ✅ 完成 | 100% | — |
| M3（原生的线） | ✅ 完成 | 100% | — |
| **M4（剩余面板）** | 🟡 主体完成 | 95% | 见 `m4.md`；仅 M4.1 像素签收卡人眼，代码与对抗评审均已收口 |
| **M5（Windows 硬化分发）** | 🟡 部分 | 65% | 见 `m5.md` |
| M6a（TUI） | ✅ 完成 | 100% | — |
| M6b（插件沙箱） | ⬜ 未开始 | 0% | 方案自标后置 |
| M7（收尾） | ✅ 完成 | 100% | — |

## 代码与测试

| 维度 | 数值 |
| --- | --- |
| Rust | 26,548 行（crates 11.0k / Tauri 壳 13.1k / TUI 2.5k） |
| TypeScript | 173,773 行（packages 61.0k / renderer 112.8k） |
| 测试 | Rust 366 + TS 332 = **698** |
| 提交 | 197 |

## 门禁（全绿）

`cargo clippy -D warnings` 六 crate · `cargo deny` 退出码 0 · contracts 2/2 ·
architecture 7/7 · `npm run check` 全绿 · Xvfb 实机启动 30 秒无 panic

## 跨里程碑阻塞

见 `blockers.md`。当前三条：

1. **无 Windows 真机** → 卡 M5.2 NSIS / M5.3 / M5.4 的全部实测项
2. **无 step CLI** → 卡「真实 turn 端到端」；omp 依赖 403 包已装，natives 的 Rust
   编译未通过（要 nightly toolchain 且磁盘曾满）
3. **像素级签收要人眼** → 卡 M4.1

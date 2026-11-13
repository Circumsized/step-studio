# 跨里程碑阻塞

## B1 — 无 Windows 真机

**卡住**：M5.2 NSIS 产线 / ed25519 更新流程 / 回滚演练；M5.3 WebView2 分发 + VC++ 运行库；
M5.4 门禁实测回填 + RSS 曲线 + 托盘跳帧 + 两种壳产物对比；M5.5 的真机用例。

**解除需要**：一台 Windows 10/11 x64（版本下限 22H2）。CI 的 windows-latest runner 可以
编译（`rust-windows` job 已绿），但不能跑 GUI，也不能验安装器。

**已做的替代**：Windows 交叉编译门禁从「一直红」修到「剩 6 个错误、全部位于
`serve_surface` 一个函数里」，且正确的 Windows 设计已记录（`set_read_timeout` 交错读写）。

## B2 — 无 step CLI

**卡住**：M0 验收④「一个真实 turn 端到端」；M4 的 Agent 状态监控数据面；`real-cli` 类测试
全部 skip。

**进展**（2026-11-13）：
- `npm run omp:install-deps` 成功，403 包装好
- `packages/natives` 的 Rust 编译（bazel/napi 路线）尚未通过——它要 nightly toolchain 且磁盘曾满
- **替代方案**：协议替身。适配器与 CLI 的契约由 `crates/step-studio-runtime/src/step_rpc.rs`
  定义（stdin 一行 JSON 请求 / stdout 一行 JSON response，其余为 event）。替身实现同一契约，
  即可驱动「壳 ↔ 适配器 ↔ CLI」三段接线——而要验的正是接线，不是 CLI 本身。

**解除需要**： natives 编译通过，或一台有 step 的机器。

## B3 — 像素级签收要人眼

**卡住**：M4.1「六页面像素级签收」、M1.4 的兼容矩阵。

**已做的替代**：Xvfb 截图 + 六页面存在性 + 每页测试。但「和 omp-studio 一致」只有人能判。

**解除需要**：项目负责人人工比对，或提供 omp-studio 的参考截图集。

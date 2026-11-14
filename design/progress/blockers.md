# 跨里程碑阻塞

## B1 — 无 Windows 真机

**卡住**：M5.2 NSIS 产线 / ed25519 更新流程 / 回滚演练；M5.3 WebView2 分发 + VC++ 运行库；
M5.4 门禁实测回填 + RSS 曲线 + 托盘跳帧 + 两种壳产物对比；M5.5 的真机用例。

**解除需要**：一台 Windows 10/11 x64（版本下限 22H2）。CI 的 windows-latest runner 可以
编译（`rust-windows` job 已绿），但不能跑 GUI，也不能验安装器。

**已做的替代**：Windows 交叉编译门禁从「一直红」修到「剩 6 个错误、全部位于
`serve_surface` 一个函数里」，且正确的 Windows 设计已记录（`set_read_timeout` 交错读写）。

## B2 — 无 step CLI —— **已解除（2026-11-14）**

**曾经卡住**：M0 验收④「一个真实 turn 端到端」；M4 的 Agent 状态监控数据面；`real-cli` 类测试
全部 skip。

**解除方式**：`npm run omp:overlay:apply` + `npm run omp:build:host` 构建出
`omp/18.13.2-studio.10`（240 MB，含 `--mode studio-host`），经 `~/.local/bin/step` 上 PATH。
构建前置：`cmake` 与 `ninja-build`（apt 安装，磁盘原先 100% 满）。

解除过程中修掉四个跨层契约错误，详见 `m5.md` 的「B2 解除记录」。其中最关键的一条：
`agent_settled` 这个回合结束事件**在 Runtime 源码里根本不存在**，适配器一直在等它，于是没有
任何回合被结算——而所有相关测试要么 skip，要么用一个同样发虚构事件的替身，所以一直是绿的。

**现在的状态**：`cargo test -p step-studio-{bridge,runtime,host,sessions,tui}` 全绿；
`real-runtime-adapter.test.js` 用真 CLI 通过。

**仍需本机之外的条件**：真实 turn 要有额度的模型。当前默认 `stepfun-cn/step-3.7-flash`；
原来的 `autoclaw-cn/zaicoding_glm-5.3` 返回 402 积分不足。

## B3 — 像素级签收要人眼

**卡住**：M4.1「六页面像素级签收」、M1.4 的兼容矩阵。

**已做的替代**：Xvfb 截图 + 六页面存在性 + 每页测试。但「和 omp-studio 一致」只有人能判。

**解除需要**：项目负责人人工比对，或提供 omp-studio 的参考截图集。

# Step Studio

在 [`the-snowpear/omp-studio`](https://github.com/the-snowpear/omp-studio) 基础上把
运行时从 OMP 换成 **Step Code**（[`stepfun-ai/Step-Code`](https://github.com/stepfun-ai/Step-Code)）
的可视化智能体平台。OMP 的 Studio Bridge 协议、host、客户端、Electron 壳全部复用；
换掉的只有"runtime 进程"这一层。

`README.md` 仍然是 OMP Studio 的说明书——它描述的复用部分依然准确。本文件只写这个
fork 的状态与差异。

## 已可用

**运行时链路已端到端跑通**（真实 Step CLI + 真实模型，不是 mock）：

```
Renderer → @omp-studio/client → transport-desktop → apps/desktop (IPC)
  → @omp-studio/host-client-api → studio-host
  → [STUDIO_RUNTIME_EXECUTABLE] → @step-studio/runtime-stepcode
  → step --mode rpc --include-live-events
```

- Studio Bridge 握手（HMAC challenge proof）、`runtime.snapshot`、`core.prompt`、
  `core.steer`/`core.followUp`、`core.abort`、`runtime.shutdown`
- `session.model.set` / `session.thinking.set` / `session.fork` /
  `session.transcript.read`，映射到 Step 的 `set_model`、`set_thinking_level`、
  `fork`、`get_entries`
- 流式正文与思考分 block 走 `conversation.message.delta`，全部经
  `parseConversationRuntimeEvent` 校验
- provider 报错时立刻返回 failed receipt 并带上 CLI 原始错误（Step 之后还会自动重试
  很久，不等它）
- 审批环（`extension_ui_request` → `interaction.required` → `interaction.respond` → `interaction.resolved`）：dialog 类 4 个 method 全通，真实 CLI 往返实证；fire-and-forget 类 5 个暂无 Studio 事件 kind，v1 丢弃（trace 可见）
- 通过 Host 的**真实探针**（hello + snapshot smoke + shutdown 后干净退出）；
  shutdown 失败会被判 rejected 而不是 limited，所以这条最硬
- 整个 `npm run check` 门禁通过：架构约束、全部包编译（含 renderer）、核心测试

## 跑起来

```bash
git submodule update --init omp-patch/vendor/oh-my-pi   # 首次
npm install
npm run omp:overlay:apply                                # 首次
npm run check                                            # 全量门禁

# 只验证运行时链路（不需要 Electron / 显示器）
cd packages/runtime-stepcode && npm run demo -- "hello"
```

接真实 Step CLI 与自定义 provider 的完整步骤见
[`packages/runtime-stepcode/README.md`](packages/runtime-stepcode/README.md)，
坑点记录在 [`packages/runtime-stepcode/docs/step-rpc-integration.md`](packages/runtime-stepcode/docs/step-rpc-integration.md)。

Tauri 壳（M1.1，dev 模式自带 Vite dev server）：

```bash
npm run tauri:dev -w @step-studio/desktop-tauri     # 等价 tauri dev
```

跑内嵌产物的 release 二进制（不需要 dev server）：

```bash
cargo build --release -p step-studio-desktop-tauri
./target/release/step-studio-desktop-tauri
```

注意 **`custom-protocol` feature**：`tauri-build` 的 `is_dev()` 读 `DEP_TAURI_DEV`，而它在
`tauri` crate 没带 `custom-protocol` feature 时为 true——此时二进制走 `build.devUrl` 而不是
内嵌的 `frontendDist`，webview 会报 "Could not connect to localhost"。`tauri build` 会替我们
加这个 feature，裸 `cargo build` 不会，所以 `src-tauri/Cargo.toml` 里显式声明了它。

Xvfb 下首帧慢（实测窗口约 30 秒后才映射），需要 `WEBKIT_DISABLE_COMPOSITING_MODE=1` 与
`WEBKIT_DISABLE_DMABUF_RENDERER=1`，且 Xvfb 要带 `-ac`（否则 GTK 报
"Authorization required"）。

桌面壳启动适配器：

```bash
STUDIO_RUNTIME_EXECUTABLE=$PWD/packages/runtime-stepcode/bin/step-studio-runtime npm run preview
```

Host 直接 spawn 这个可执行文件并传 `--mode studio-host ...`，所以它不能是 Node 脚本。

不设该变量时行为与 OMP Studio 完全一致（仍走 managed 安装的 OMP 运行时）。

## 原生 Rust 壳（无 webview）

Electron 的 Chromium 进程占整个应用 280–470 MB。`crates/` 下新增 egui/winit 壳把它们
全部去掉：无 webview、无 Electron，release 二进制 16 MB。

分工是故意的：**协议、平台、壳、适配器全部 Rust**，TypeScript 只剩 Renderer。此前
端到端跑通的 TypeScript 适配器保留为回退，壳优先用 Rust 版本，没构建出来时才落到 Node。

```
crates/studio-bridge/       wire 协议：帧编解码、HMAC 握手、会话客户端
crates/step-studio-runtime/ Studio Bridge 适配器：驱动 step --mode rpc
crates/step-studio-shell/   应用：后台桥接线程 + 纯归约器 + egui 视图
```

```bash
cargo build --release
./target/release/step-studio-shell            # step 自己从 PATH 或 ~/.stepcode/bin 找
./target/release/step-studio-shell --prompt "hello"
```

壳已经是可以直接用的聊天界面，不是协议探针：

| 能力 | 状态 |
|---|---|
| 流式正文 / 思考分块 | 实时逐字，不等到回合结束 |
| markdown | 标题、列表、代码块、行内 `**粗体**` / `` `代码` `` 都有真实样式（egui 没有 markdown 控件） |
| 模型切换 | 顶栏下拉，来自 CLI 的实时 catalog |
| 停止 | 流式中 Send 变 Stop，发 `core.abort` |
| 新会话 | 顶栏 New，调 Step 的 `new_session` |
| 会话续接 | 重启后自动 `--continue`，从 transcript 重建历史 |
|  thinking 档位 | **没有 UI**：当前模型只报 `off` 一个档位，manifest 里已如实标 limited |
| 扩展 / MCP / 命令 | 顶栏 `Catalog` 抽屉：读 `step.plugin.json` 的 `mcpServers` + 一次 `get_commands` RPC |

`cargo test` 41 个测试，含对真实适配器与真实 CLI 的端到端。

帧编解码与 `@omp-studio/studio-protocol` 逐字节兼容（大端长度前缀、五键帧头、HMAC
challenge proof）。第一版我写成了小端 + 只含 body 的长度 + 两键头——对着 mock 全能过，
是对着真实适配器的端到端测试抓出来的。

实测（release，Linux，**官方安装的真实 `step` 0.1.7**）：

| 进程 | RSS |
|---|---|
| 壳（Rust/egui） | 118.0 MB |
| 适配器（Rust） | 2.8 MB |
| step CLI（真实二进制） | 167.1 MB |
| steppage-mcp（Step 自己拉的） | 84.9 MB |
| **应用合计** | **372.8 MB**（聊天子集口径：壳直连适配器，无 host、无 renderer、无 workspace/git/usage；不是全功能预算） |

同一链路换回 TypeScript 适配器是 118 + 64.4 + 167 + 85 = **434 MB**，即 Rust 适配器
省掉 61 MB（14%）。壳在 `target/{release,debug}/step-studio-runtime` 存在时优先用它，
否则回退 Node 适配器。

（另加 Xvfb 67 MB，仅无头测试需要，Windows 上没有。）

性能开销方面 CPU 几乎为零：一次回合 5.7 秒里整个进程树只用 0.2 秒 CPU（单核
3%），瓶颈完全在等模型返回。冷启动到首个快照 3.4 秒，绝大部分是 step 自己启动。

> 数字订正过两次，都记下来以免再犯：
> - 322 MB 是 `pnpm step` 走 tsx 现编 TypeScript 的**开发路径**，tsx + pnpm 的纯开发
>   开销 225 MB。
> - 257 MB 是**源码 clone 构建出 dist** 后跑 `node dist/main.js` 的路径。
> - **160 MB 才是官方安装的真实二进制**——打包成单文件可执行文件（114 MB），
>   比 node + 一堆模块文件省 100 MB。

Electron 那 280–470 MB 是实打实省掉了。剩下的 372.8 MB 里运行时占 68%，且已经榨到
底：

- **适配器 2.8 MB**：原来是 64 MB 的 Node 进程，只做帧编解码 + JSONL 转发，约 600 行
  逻辑，已用 `crates/step-studio-runtime/` 重写。
- **step CLI 167 MB + MCP 85 MB**：Step Code 自身，要动就得改上游。
- **壳 118 MB**：egui/winit 加 Mesa 软件光栅化（无头 Xvfb 下）。真实 GPU 上更低。

Windows 的传输已接上：`crates/studio-bridge/src/transport.rs` 是唯一放平台差异的地方，
Unix domain socket / Windows 命名管道。两个目标都过 `clippy --all-targets -D warnings`
（33 个 Unix 测试照旧通过，Windows 那边 CI 新增了一个 job 只做编译和 lint）。

**Windows 上的运行时行为仍未验证**——没有真机，交叉编译证明不了握手、帧编解码和
`--continue` 在管道上真的能跑。这一条从「编译不过」变成了「编译过但没跑过」。

## Tauri 壳（M1.1）

`apps/desktop-tauri/` 是替代 `apps/desktop/` 的 Tauri v2 主壳：单个 WebView 承载既有的
React renderer，进程内直连（M1.2 起接 `studio-bridge` facade）。M1.1 只做「脚手架 +
renderer 挂载 + bridge 注入」，不含任何面板迁移。

```
apps/desktop-tauri/
├── src/preload.ts              init script：createDesktopIpcBridge(tauriSender) → window.ompStudio
├── src/tauri-sender.ts         Tauri invoke/listen → DesktopIpcBridge sender
├── test/                       18 个无头测试（无 Tauri 运行时、无 WebView）
└── src-tauri/                  Cargo.toml、main.rs、bridge.rs（六个 stub）、capabilities、icons
```

三个关键点：

- **renderer 零改动**。`apps/renderer/src/main.tsx:74` 本来就读 `globalThis.ompStudio` 自己建
  client，所以壳唯一的任务是注入那个 global。`createDesktopIpcBridge` 已从
  `apps/desktop/src/ipc-validation.ts` 移进 `packages/transport-desktop/src/ipc-validation.ts`
  并再导出——Electron preload 与它的测试都改为从包里导入，桥本身一行未改。
- **init script 走 initialization_script，不走 renderer 侧 import**。让 renderer 直接
  `import { invoke } from "@tauri-apps/api/core"` 会同时破坏 preload 声明的三条不变量
  （无通用 invoke、无 IPC 泄漏、桥是唯一桌面 API）。代价是 Tauri 没有 contextBridge，
  init script 跑在主世界，所以 `Object.defineProperty` 显式设为不可写——这是加固不是边界，
  真正的不变量按方案 §8 放在 Rust facade。
- **M1.1 只注入 `ompStudio`**。`ompStudioChrome`/`ompStudioTerminal`/`ompStudioRemote`
  三个 global 留给 M1.2；renderer 对三者全是可选调用，缺省即降级（性能上报变 no-op、
  平台改从 UA 推断、终端面板禁用、remote 面板进入本地模式）。

已验证：`cargo check` / `cargo clippy --all-targets -D warnings` 干净（`[lints] workspace = true`
没有触发 `unsafe_code = "forbid"`）；18 个单测全绿并进了 `scripts/core-suites.mjs`；全量
`npm run check` 绿（新增 `build:desktop-tauri` 与 `test:desktop-tauri` 两项）；Xvfb 下
release 二进制跑通了整条链路：窗口约 30 秒后映射，首屏是完整的工作台（侧栏、Composer、
底部面板、顶部 "Runtime 不可用" 横幅与 "Failed to load usage"——stub 返回的诚实降级态）。
工作台而非 `<Unavailable />`，证明 init script 先于 renderer bundle 执行、bridge 打通。

 smoke 抓出两个只有真机能发现的问题，都已修：

- **`custom-protocol` feature**：`tauri-build` 的 `is_dev()` 读 `DEP_TAURI_DEV`，而它在
  `tauri` crate 没带 `custom-protocol` 时为 true——二进制会走 `build.devUrl` 去连 Vite，
  webview 报 "Could not connect to localhost"。`tauri build` 会替我们加这个 feature，
  裸 `cargo build` 不会。
- **`security.freezePrototype`**：开了它，renderer bundle 在 import 阶段就抛
  `Attempted to assign to readonly property`（ES module 严格模式下直接让整个 module
  不执行），页面全白。Tauri 自己的文档就写着它会让 polyfills 与部分框架失效。方案 §8
  从没要求这层硬化，已移除。

init script 先于 renderer bundle 执行这一点已由上面的 smoke 实证（WebKitGTK 口径）。

未验证（按方案 §16.4 口径如实记）：WebView2 上的行为（init script 是否同样绕过 `script-src`、
DPI 150%/200%、IME、xterm.js 帧时与 RSS）；`cargo test --workspace` 并行跑时 e2e 偶发一失败，
单独跑 8/8 过、复跑两次全绿，未定位到具体用例（最可能是打真实 CLI 与模型 API 的那个在并行
负载下超时），记为已知抖动。

### 续接是怎么接上的

Step 按 project 存 session，所以壳传给 agent 的工作目录**必须跨启动稳定**——
最初用的是 per-PID 临时目录，每次启动都是新 project，`--continue` 永远找不到上一个
session。现在用 repo 根目录。

适配器通过 `STEP_STUDIO_SESSION=continue` 选择性开启；桌面 Host spawn 时不带这个变量，
行为不变。

## 尚未完成

- **Windows 命名管道与打包实测**：适配器只在 Linux 上验过 Unix domain socket；
  `npm run pack:win` 未跑
- **品牌替换**：窗口标题、关于对话框、`%APPDATA%\<name>`、安装器产品名仍是
  `omp-studio`。包名（`@omp-studio/*`）不改——它们是复用的依赖，改名会破坏整个
  依赖图和架构约束
- **其余命令映射**：`turn.retry`、`session.drop`、`session.clearContext`、
  `session.handoff`、`mode.plan.*` 未实现；`switch_session`/`clone`/`get_tree`/
  `get_messages`/`get_session_stats`/`compact`/`bash` 等未接（适配器缺口清单
  见 [`design/plan.md`](plan.md) 11.4）
- **审批 UI**：Rust 壳与 renderer 都还没有审批卡片界面，适配器侧契约已通、
  界面侧未接（M1 起）
- **Agent Hub 的子 agent 编排**：Step 有并行 subagent，但 RPC 没暴露列表与状态
- **Step 没有的能力**：`tui.transfer`、`vibe.mode`、`omfg.commit`、`prewalk`、
  `operator`、`tan` 标为 `unavailable`，Renderer 会隐藏它们
- **用量面板**：`step.usage_provenance` 条目里已有 token/耗时/成本，尚未接到界面

## 结构差异

| 路径 | 说明 |
|---|---|
| `packages/runtime-stepcode/` | 新增：Step Code 的 Studio Bridge 适配器（TypeScript，现为回退路径） |
| `crates/step-studio-runtime/` | 新增：同一适配器的 Rust 实现，壳优先使用 |
| `crates/step-studio-shell/src/markdown.rs` | 新增：markdown 行分类，与渲染分开以便无窗口测试 |
| `apps/desktop/src/host-composition.ts` | 改：`STUDIO_RUNTIME_EXECUTABLE` 选择 custom runtime 源，并让显式指定的 limited 运行时获得 session controller |
| `apps/desktop-tauri/` | 新增：Tauri v2 主壳（M1.1 脚手架 + bridge 注入 + 六个命令 stub） |
| `packages/transport-desktop/src/ipc-validation.ts` | 新增：`createDesktopIpcBridge` 等从 `apps/desktop/src/` 迁入，桥本身零改动 |
| `apps/desktop/src/preload.ts` | 改：桥工厂三件套改为从 `@omp-studio/transport-desktop` 导入 |
| `scripts/architecture.test.mjs` | 改：`transport-desktop` 允许依赖 `client`；`sourceFiles` 跳过 `target`；electron 豁免按路径而非 key 判定 |
| `scripts/core-suites.mjs` | 改：登记 desktop-tauri 两个测试 |
| `package-lock.json` | 修：补回缺失的 `packages/runtime-stepcode` 条目（此前任何 `npm install` 都会失败） |
| `package-lock.json` | 改：登记 `@step-studio/runtime-stepcode` workspace 链接 |
| 其余 | 与上游一致 |

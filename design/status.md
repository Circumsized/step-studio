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

## 原生 Rust 壳（无 webview）——**已被 v5.0 架构决策取代**

> 用户 2026-11-11 定「外壳使用纯 Tauri」，egui 与 gpui 两个原生面删除。本节描述的实现
> （`crates/step-studio-shell`，1,993 行）随之成为**死代码，待删**。保留原文仅供删前参照
> 与历史查证，不再代表产品形态。
>
> 删除前要注意的代价：该 crate 承载 **22 个 cargo 测试**（markdown 行分类 7、扩展面板 3、
> 其余壳内逻辑），删后工作区测试从 41 降到 19。死代码的测试随代码一起消亡是正确的，
> 但 `plan.md` 与本节里所有「41 个测试」的口径要在删除后同步修订。
>
> 它的一项职责已有替代：**无显示环境的 E2E 夹具**——M1.1 已实证 Xvfb + Tauri release
> 二进制可跑通整链路（见下节），不再需要 egui 壳兜底。

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
底部面板）。工作台而非 `<Unavailable />`，证明 init script 先于 renderer bundle 执行。

### M1.2a：bootstrap 接真实会话（2026-11-11 完成）

`bootstrap` 从 stub 改为接真实 `BridgeSession`：连接适配器、取 snapshot、翻译成
`ClientBootstrap`。**Xvfb 下状态栏从「Studio ● Unavailable」变成「Studio ● OMP Ready」**，
两条错误横幅消失，工作台完整渲染（usage 与麦克风两项仍报不可用，属 M1.2b/c）。

途中修掉三个只有真机能抓的问题：

1. **`native_runtime_command` 重复候选污染 argv**：原版 egui 壳是「首个匹配即返回」，我移植时
   "简化"成 `.filter().map().collect()`，而 beside_exe 与 `target/release/` 命中同一个文件，
   于是 `AdapterLaunch.command` 有两个元素，`spawn` 把第二份当成参数传给适配器，报
   `unknown argument <自身路径>`。
2. **`app.manage()` 存了 `Arc<SessionState>` 而命令要 `State<SessionState>`**：Tauri 按类型存取，
   包装一层就全部 miss，renderer 收到 "state not managed"。
3. **Studio snapshot ≠ ClientBootstrap**：直接把 snapshot 返给 renderer 会得到
   "Client contract mismatch: expected 2, received undefined"——两者形状不同，必须翻译。

这三个都能编译过、都能被单测漏掉，只有跑起来才暴露。

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

### M1.2c 完成：remote 桥不建，方案非目标（2026-11-12）

M1.2c 三条桥，chrome 与 terminal 已落地（见上面两节）。第三条 `ompStudioRemote`
**不建**——方案 §1.3 非目标明文写着「不做远程工作区、手机连接、插件市场、delta 更新」。

不建的理由不是工作量，是范围：`apps/desktop/src/remote/` 是 2416 行 / 23 个文件的
SSH 子系统（部署、网络代理、本地 vault、provisioning、环境安装），而 renderer 只用到
其中 6 个方法（`list` / `connect` / `select` / `onChanged` / `approveDeployment` /
`disconnect`）。为一个非目标搭 2416 行基础设施没有意义。

`ompStudioRemote` 因此保持缺席，renderer 的 remote 面板走 M1.1 已确立的降级路径
（本地模式）。这与 chrome 的 Host 背后方法留空是同一条纪律：**缺席即诚实不可用，
不伪造**。

M1.2 至此完成：6 个 Desktop IPC channel 全部接上真实会话，三条桥按方案范围裁剪落地。

### M1.3 窗口铬 / Vite base / CSP / DPI（2026-11-12 完成）

四项里两项查证后确认无需改动，一项实测通过，一项只测了一半——分开记。

**Vite base：无需改动。** `apps/renderer/vite.config.ts:35` 已是 `base: "./"`（相对路径），
Tauri 的 `WebviewUrl::App("index.html")` 下相对资源正确解析。截图已实证资源加载正常。

**CSP：无需改动，且确认是单点。** 策略由 renderer 自己在构建期注入
`<meta http-equiv="Content-Security-Policy">`（`vite.config.ts:16-28` 的
`packagedCspPlugin`），Tauri 侧 `security.csp` 保持 `null` 不加第二份。一处真相，
不会出现两份策略互相覆盖。

**窗口铬重校：没有要删的东西。** 原先以为顶部「File / Edit / View / Help」是 GTK 原生
菜单栏、纯 Tauri 会带进来、需要删掉。查证后**这个判断是错的**：Tauri 与 tao 的源码里都
不建菜单栏，那条栏是 renderer 自己画的 `TitleMenu`（`App.tsx:8701-8732`），属于应用
自定义标题栏、也就是要复刻的 UI 本身。所以窗口铬这一项无事可做。

**DPI 150% / 200%：实测通过，但只测了一半，必须说清。**
Xvfb 分别以 `-dpi 144` 与 `-dpi 192` 起显示，跑 release 二进制并截图：两档下侧栏、
composer、底部面板、转写全部正常，无裁切、无缺失文本、无布局破裂。

**局限（这条比结论重要）**：Linux/Xvfb 的 `-dpi` 只改变 X 服务器上报的 DPI，WebKitGTK
用它做物理尺寸换算，**CSS 像素布局不变**。而 Windows 的「150% / 200% 显示缩放」是另一
回事——操作系统施加缩放，WebView 上报对应的 devicePixelRatio，应用看到的是物理像素变多。
所以这次证明的是「不同上报 DPI 下不破裂」，**不是**「Windows 显示缩放下正确」。
方案 M1.3 真正要求的 WebView2 DPI 两档仍未验证，需要真机。连同 init script 是否同样
绕过 WebView2 的 `script-src`、自定义协议在 WebView2 上的资源服务、桥注入时机，
一起记在未验证清单里。

### 改名：OMP → Step（2026-11-12 完成）

用户要求的复刻条件里有一条是「除把 omp 改为 step之外基本复刻」。此前只有 Tauri 窗口标题是
Step Studio，界面内全是 OMP。本次把**可见文案**里的 OMP 全部改成 Step：侧栏 brand、就绪状态、
控制台标题、诊断报告头、审批卡片主语、各设置页对 Runtime 行为的说明、`<title>`。

共 207 处、51 个文件，外加 `index.html` 的 `<title>`。

替换只针对**独立大写词** `\bOMP\b`，这一条限定就是安全性的来源：
- `OMP_STUDIO_CLIENT` 不动——`_` 是词字符，`P` 后没有词边界；
- `omp-studio`、`@omp-studio`、`ompStudio` 全小写，永不匹配；
- 真实配置路径 `~/.omp/agent/config.yml` 全小写，也不会被误伤。

**两个标识符故意不改**，都记录在案而不是默默跳过：
- `tauri.conf.json` 的 `com.snowpear.omp-studio`。改它等于迁移安装目录与 app-data 目录，
  会让已有 profile 失效。它不是界面显示。
- `OMP_STUDIO_CLIENT`，跨语言注入点。

44 条测试断言跟着改——它们钉的就是显示文案，文案动了它们必须动。

实机验证：侧栏「Step Studio」、状态「Step Ready」、底部页签「Step Logs」。
**踩到一个只有真机能发现的问题**：改完 dist 后界面仍是旧文案，原因是 Tauri 在**编译期**把
`frontendDist` 嵌进二进制，光重建 renderer dist 不够，必须重新 `cargo build --release`。
同理 `index.html` 的 `<title>` 也在嵌入范围内。

### resync 横幅回归与修复（2026-11-12 完成）

改名后实机截图暴露出一个**新回归**：界面顶部常驻
「Resync required · Restoring latest Runtime state; sensitive operations are paused.」

根因：bootstrap 把 `cursor: 0` 声明为订阅续起点，但泵的计数器对**被丢弃的事件也照常前进**——
而 `subscribe` 之前的事件全部被丢弃（适配器启动chatter、self-check 回合）。于是第一个真正
送达的事件在 renderer 眼里成了「间隙」，而间隙会强制 `resync_required`
（`packages/client/src/reducer.ts:737`）。这个壳从不发 snapshot 事件，所以横幅再也不会消。

修法：sink 现在回报「是否真的送达」，两条派发路径在未送达时把流回滚。回滚覆盖**两个字段**
而不只是一个：`translate()` 在合成回合结束回执时会 `.take()` 掉 `pending_turn`，只回滚 cursor
会留下一个「回执已构造、已丢弃、且永远不能再构造」的 prompt。

三个测试钉住这个不变量，其中 `pending_turn` 那条正是第一版修法写错的地方——`checkpoint()`
最初用 `.take()` 实现，把配对关系偷走了，`translate()` 从此看不到它。测试先红，改成 clone 后才绿。

### Host 归属决策的实测数据（2026-11-12）

用户要求按四个标准裁决 Host 逻辑放哪：高性能低开销、step cli 原生化、Windows 优化、
保持 omp studio 逻辑。先把可量化的测出来，再让专家按这些数字审议。

**进程树实测**（Xvfb，Linux，release 二进制，干净启动无残留进程）：

| 组件 | RSS | 占比 |
|---|---|---|
| WebKitWebProcess（renderer） | 501.0 MB | 55.2% |
| Tauri 壳（Rust） | 183.3 MB | 20.2% |
| step CLI（114 MB 二进制） | 168.5 MB | 18.6% |
| WebKitNetworkProcess | 50.4 MB | 5.6% |
| step-studio-runtime（Rust 适配器） | 4.0 MB | 0.4% |
| **合计** | **907.2 MB** | |

**边车实测（两轮，第二轮推翻了第一轮）**：

第一轮只量了「加载后空闲」：裸 `node` 42.3 MB，加载完整 Host 组合图后 79.2 MB。
Bun 编译版（238 模块，89.1 MB 单文件）空闲仅 **35.8 MB**。曾据此后一个结论，是错的。

第二轮按真实热路径重测——Host 自己的 `ConversationEventFanout` 工作负载放大、40 个真实
`git` 子进程、20 个 fs watcher、三轮 soak，并且**在循环内每 2000 次操作采样一次**加外部
20 ms 进程树采样（第一阶段的分界点采样抓不到峰值，峰值落在循环中段）：

| 阶段 | Node | Bun 编译版 |
|---|---|---|
| 空闲（全图加载后） | 83.5 MB | 22.5 MB |
| 25 万条 text delta 后 | 148.8 MB | 72.2 MB |
| 10 万次 tool-output 追加后 | 267.2 MB | 123.5 MB（自峰 145.5） |
| 20 万次重复解析后 | 287.0 MB | 113.6 MB |
| 三轮 soak 后 | 324.8 MB | 90.1 MB |
| **真峰（外部 20 ms 树采样）** | **351.6 MB** | **157.1 MB** |

所以边车的诚实数字是 **Bun 峰值 145-157 MB，Node 峰值 352 MB**——不是 35.8，也不是 79.2。
方法学教训：分界点采样会漏掉峰值，必须循环内采样 + 外部采样互校。

但有两个对 Bun 有利的定性差异，比绝对值更重要：**Bun 峰值比 Node 低 2.2 倍、空闲低 3.7 倍；
而且 Bun 在负载后会把内存还回来（145 → 90 MB），Node 不会（287 → 325 MB，单调上涨）**。
前者是量级差异，后者是泄漏与否的差异。

**一个此前的错误认知，实测纠正**：曾以为「Node 已经在进程树里」（依据是
`packages/runtime-stepcode`）。不成立——当前壳 spawn 的是 **Rust 适配器**
`step-studio-runtime`（4.0 MB），Node 适配器只在找不到 Rust 二进制时才是回退。
所以边车方案是**新增**一个进程，不是复用已有的。

### 编译 Host 成单文件：一个绿灯构建的地雷（实测）

Bun 能把**含 `host-factory.ts` 的完整图**（269 模块）编译成 154 MB 单文件，零警告，
二进制能启动，两个导出类型正确——看起来完全成功。它不是。

`host-factory.ts:41` 是 `import { app, dialog } from "electron"`，而 electron 包的
`index.js` 末尾是 `module.exports = getElectronPath()`——**一个字符串**。Bun 的 CJS 互操作
从这个字符串上解构 `{ app, dialog }`，两者都是 `undefined`，于是：

```
typeof app = undefined
LANDMINE -> TypeError: undefined is not an object (evaluating 'import_electron.app.isPackaged')
```

命中 `host-factory.ts:500` 与 `:196`。**构建绿、二进制能起、导出类型正确，然后一碰平台端口就死**，
而且没有任何信号——不是来自打包器、不是来自 typecheck、也不是来自只 import 的 smoke run。
更糟：这个 89 MB 二进制还悄悄带着 Electron 的安装 shim，缺 `path.txt`/`dist` 时会在启动时
触发一次约 100 MB 的 Electron 下载。

所以「复用 100% 代码」在两处不成立：
1. 那 3 个 electron 调用点要补一个 Tauri 平台端口实现（`app.isPackaged` ×2、
   `process.resourcesPath` ×1，加 `dialog.showOpenDialog`）；
2. 方案 §10.2 列在 Host 里的 PTY 池实际在 `apps/desktop/src/terminal-pty.ts`，
   **在组合图之外**（其文件头自称「never talks to the Host」）且依赖原生 `node-pty`。
   复用 Host 图拿不到它。

**step 原生 RPC 覆盖面对比**：`step --mode rpc` 只暴露 10 个方法——
`prompt` / `new_session` / `fork` / `get_state` / `get_entries` / `set_model` /
`set_thinking_level` / `get_available_models` / `get_available_thinking_levels` /
`get_commands`，全部是对话面。契约里共有 249 个 operation，facade 有 105 个分派分支
（46 query + 51 command）。**step 原生覆盖 10 个，其余约 150 个是 Host 独有**：
多会话列表与归档、workspace 注册表、git、models.yml 管理、MCP/skills 发现、
Agent Hub / subagent、telemetry。适配器自己的注释也写着「Step Code has no OMP
operators, agents, jobs or permissions model to expose」。

这与复刻目标直接冲突：九张基准图里的 hub / roles / capabilities / telemetry / git
全是 Host 数据面。所以「全靠 step 原生化」这条路被实测否掉——step 的 RPC 不是为
桌面控制台设计的。

**另一个警讯**：G-1 门禁的前置条件是「WebView2 + renderer 实测 ≤ 165 MB」，而我实测
renderer 501 MB（Linux/WebKitGTK，与 WebView2 不可直接比，但量级差 3 倍）。方案自己
认定的内存第一杠杆是**终端原生化**，不是 Host。若 G-1 前置条件真不成立，该先查的是
renderer 与终端，而不是省 Host 这 79 MB。

### Host 边车方案查证结论（2026-11-12，尚未实施）

M1.2c 开工前先回答了一个问题：chrome 的 40 个 Host 背后方法，值得逐个移植进 Rust 吗？
查证结论是**不值得，而且没必要**——`createDesktopHostComposition` 可以在无 Electron 的普通
Node 进程里驱动：

- `host-composition.ts` 自身零 electron import；全仓只有 `host-factory.ts:41` 一条
  `import { app, dialog } from "electron"`，且只被 3 个调用点用到（`app.isPackaged` ×2、
  `process.resourcesPath` ×1），都是普通参数而非依赖。
- 实证：在裸 Node 里跑既有契约套件 `node --test apps/desktop/dist/test/desktop-composition.test.js`
  → 22 过 1 跳 0 失败，且该套件用的是真的 `createDesktopRuntimeSessionPort`。
- 七个必填 port 的实现（platform / authorityLock / privateEndpoint / runtimeSession 等）
  全部在 `@omp-studio/platform` 与 `apps/desktop/src` 的非 electron 模块里，electron-free。
- **五方法面由 composition 自己构造**：`host-composition.ts:1106-1108` 的
  `get transport() { return this.#facade; }`，而 `#facade` 就是
  `StudioHostClientFacade implements ClientTransport`，后者恰好是
  `{bootstrap, query, command, subscribe, close}`。Electron 只在这之上加 IPC 转发。

也就是说：起一个 Node Host 边车、Tauri 壳只做转发，可以一次拿到全部约 160 个操作，
**renderer 零 diff**。工作量估算：约 200-300 行新 TS 边车胶水 + 约 350 行从 electron 模块里
搬出来的生产服务 + Rust 侧约 250-400 行代理（替代现有 603 行）。

两个已知代价，记下来免得后面才发现：
- `transport-web` 的 HTTP 服务不能直接复用：没有 WebSocket 服务端（事件流要新写约 60-100 行）、
  `origins` 必填且各平台不同、配对码一次性 60 秒 TTL、默认 `maxOutstandingCommands: 8`。
  走 stdio NDJSON 更干净。
- 两个壳共用同一个 per-profile authority lock（`environmentKey: "desktop"`）。Electron 与
  Tauri 同时跑会互斥；要并行就得让 `environmentKey` 按壳区分。

这条已写入 `design/plan.md` §19 决策日志，作为 M1.2c-M1.7 复刻阶段的路线选择。

### M3.3 第一刀：`git.repository.get` 落地（2026-11-12）

Git 面板 12 个 query 里的第一个，也是渲染层真正的入口（`GitStatusPanel.tsx:58`）。由**壳本地作答**——它是工作区的事实，不是 runtime 的事实，适配器没有 git 能力可提供。

§9.6 的目标是「gix 读 + CLI 写」。第一刀用 CLI 读：`git status --porcelain=v2 --branch` **一次调用即覆盖 `GitRepositoryReadModel` 的必填项**（分支、upstream、ahead/behind、每个文件的 index/worktree 双侧状态），外加一次 `git stash list` 计数。当天可对真仓库验证——6 个单测里有一条直接打本仓库，断言 branch/headOid/stashCount 都拿得到。

gix 仍记为目标，留到子进程成本在某个测量里显现。这是 §10.3 的逐个案通道，本案的实测依据就是「一次 porcelain 覆盖必填项」。

**checklist 3.3 翻「齐」**（提交历史：oid/parents/subject/author/date/refs/relation 全覆盖）；3.1 部分翻转（变更列表已到桥，diff 视图/暂存/提交未接）；3.2 分支列表未接。

同一刀加做 `git.log.list`：与 `repository_get` 同形，`git log` 带 `%x1f`/`%x1e` 分隔符一次给出 `GitLogCommitRecord` 全部必填项。多取一条来判断 `truncated`——「还有更多」而不是「正好这么多」。修掉一处自己引入的类型错误：`GitLogRefKind` 是 `local`/`remote`/`tag`/`head`，我第一版写了 `branch`。

### M3.2 / M3.3 起点实证（2026-11-12）

**M3.2 进程树**：契约里没有对应 query，九张基准图里也没有它，方案自己写「无像素级义务」。所以它不是接一个已有面，而是新设计一个面。唯一已确认的真实数据源是 **daemon 自己的进程树**（daemon → 适配器 → step），OS 事实而非 RPC 事实；「用户机器上所有相关进程」没有数据源（Step RPC 不暴露 subagent 列表，`hub.png` 早已记为数据源缺失）。定案前不动 UI。

**M3.3 git 面板**：`git.png` 是九张基准图之一，所以它**有像素级义务**，与 M3.2 不同。方案 §9.6 定「gix 读 + CLI 写」，12 个 arm + gix 依赖 + 子进程管理 + 路径穿越安全检查，估 6-10 人日，是 M3 最重单项。验收可直接用 `comparison-checklist.md` 的 git 段——那张表已逐点列好。

### M3.3 写侧：`git.execute` 落地（2026-11-12 完成）

契约的 `GitOperation` 有约 30 种 kind。实现 Git 面板变更列表真正发出的三个——`stage` / `unstage` / `commit`——**其余明确拒绝并在 message 里说明未接**，而不是装作操作面是全的。渲染层把 message 显示出来，所以用户看到的是「未接」而不是静默无操作。

路径安全：每个 path 与分支名都是单 argv 元素、不过 shell；前导 `-`（会被当成选项）、绝对路径、含 `..` 段的一律拒绝。`commit` 空 message 拒绝；`sign` 默认关（默认签名的壳在没有密钥的机器上每次提交都会失败）。

第二批接上四个 branch operation，第三批接上 fetch/pull/push 与 stash.push/stash.apply，第四批接上 stash.drop/discard/merge/rebase/cherry-pick/revert/reset/checkout/tag.create/tag.delete/remote.add/remote.setUrl/remote.remove/worktree.{create,lock,unlock,remove,prune}。

**至此 Git 面板 action 菜单发出的 git operation 全部接上**（`pr.*` 走 `github.execute`，不属于这个面）。剩 `init`/`clone`/`continue`/`abort`/`cancel` 五种面板并不发，保持明确拒绝。

破坏性操作一律带乐观并发检查：`discard`、`reset`、`stash.drop`、`worktree.remove`、`branch.delete` 都比对 `expectedRevision` 与当前 head，不一致就拒绝——head 移动过，调用方看到的就不再是将被操作的那个。`reset` 的 mode 只认 soft/mixed/hard 三种。

force push 只以 `--force-with-lease` 发出，**从不发裸 `--force`**——lease 会在远端移动过调用方看到的 oid 时拒绝推送，这才是阻止 force push 丢掉别人提交的东西。argv 构造提成 `push_argv` 纯函数，使这条规则不需要远端就能测。`pull` 的 strategy 只认契约那三种，其余拒绝而不是静默变成 merge。

过程中修掉一个**有副作用的测试**：`an_unwired_operation_says_so` 拿 `push` 当「未接」的例子，而我刚把 push 接上，于是它真的去推了远端（403 挡住，没推成）。改用真正未接的 `merge`——未接的例子必须真的是未接的，否则测试会把它执行一遍。其中 `branch.delete` 的 `expectedRevision` 是契约的乐观并发检查：调用方看到的是某个 head，若 head 已经移动，用户想删的分支未必就是将要被删的那个，所以拒绝而不是猜。有单测用一个假 revision 钉住「拒绝」而非「猜」。

`git.execute` 走泵的 `Dispatch::Local` 而不是在 `command` 里直接答——渲染层的 `execute` 是「先收 ack，再等终态回执」，而泵的 local 臂正好产出这两个。第一版我在 `command` 里直接返回，那会让 `waitReceipt` 永远等不到回执。

测试：18 个 git 单测，含对真仓库的 stage→unstage 往返；scratch 文件测完即清（工作树干净时没有现成 diff 可测，而改已提交文件再还原会在测试失败时留下脏树）。

### M2.3 多面第一块：daemon 自己的进程树（2026-11-12 完成）

进程树只能由持有它的进程报告——daemon 被 `process_group(0)` 拉成自己的 session，壳重启后它同 PID 存活，即壳侧遍历 `/proc` 永远看不到它。这是 M3.2 查清的结论，这块是它的数据源。

`crates/studio-host/src/process.rs`：以 daemon 为根广度优先遍历，**角色由树里的深度决定而非可执行文件名**（daemon → 直接子进程是 adapter → 再往下是 CLI）；猜一个二进制是什么，改个名就错。RSS 从 `/proc/<pid>/status` 取；平台描述不了的进程其 rss 缺席、总数就短，模型带 `complete` 标志说明这点，不让一个偏少的总数看起来像精确值。

Windows 无 `std` 可达的 toolhelp 快照（要 `unsafe`），所以那边只描述 daemon 自己，`complete: false`。

过程中两个测试自身的问题：`sh -c 'sleep 30'` 会 **exec 替换自己**、根本没有孙子进程，测试会是空转（改成两条命令）；以及 `tree()` 在 spawn 后立刻调用，竞态抢在 shell fork 之前。

**已接线**（下一步同批完成）：契约加 `process.tree.get` + `ProcessTreeReadModel`；daemon 在转发途中拦截它、自己作答，回执经**专职写线程**写到 surface 的写半边（relay 线程不碰那个 fd，两个 owner 会交错帧）；壳把它当普通 operation 发送——它走的是同一条 wire，只是目的地址是 daemon。测试断言树以 daemon 为根、含适配器、总数是上报值之和。

这就是 M2.3「多面」的第一道缝：daemon 第一次终结一个 operation 而不是透传。

**同批打通多面本体**：把「一个 slot」改成「一组 surface 接收者」，适配器的广播真正扇出；并拆掉「同时只服务一个 surface」那道闸——它的存在是因为两个 surface 会把一路广播劈成两半，而扇出之后该修的是广播，不是把第二个 surface 挡住。surface 用自增 serial 作身份（`mpsc::Sender` 无可比身份），pump 退出只清自己的条目。

测试：两个 surface 同时挂着，一个跑回合，另一个**看到同一个答案**（不是被劈走的那一半），且能继续驱动同一个会话。这正是评审 R2/R3 预警的那条，现在有对拍。

### D-1 定案：Step 的 session 存储键就是 project 绝对路径（2026-11-12 实测）

D-1 的判据本就是个实证问题，直接查 `~/.stepcode`：

```
~/.stepcode/agent/sessions/
├── --home-ubuntu-workspace--/
├── --home-ubuntu-workspace-step-studio--/
│   └── 2026-11-11T19-58-57-822Z_01a1cc31-....jsonl   （内含 cwd = /home/ubuntu/workspace/step-studio）
├── --tmp-m0-probe--/
└── ...
```

每个项目一个目录，目录名是绝对路径把 `/` 换成 `-`，session 文件内另带 `cwd` 字段。**即每个 worktree（不同绝对路径）天然落到各自的 session 存储**，`--continue` 按 project 各自恢复。

定案：**每个 worktree 作为独立 project 传给适配器**，不需要 Step 有 worktree 概念；首页项目列表可以是多 project，与适配器不冲突。这解开了 M2.3 的三重阻塞之一（另两重是会话目录移植与多面）。

### M7 第一块：删除死代码 `crates/step-studio-shell`（2026-11-13 完成）

方案自 v5.0（外壳定纯 Tauri、删除 egui 面）起就把它标记为「死代码待清」——它的无头夹具
职责已由 Xvfb + Tauri release 二进制承接（M1.1 实证）。删除前核实**无任何外部依赖**：
全仓只有 workspace `Cargo.toml` 成员表和 `bridge.rs` 的一条文档注释提到它。

1993 行、10 个文件。删除后 `cargo build/test --workspace` 与 `npm run check` 全绿，
无残留引用。

**M7 剩余部分仍 gate 在 M5.6**：`platform-darwin` 被 Electron 主进程
（`apps/desktop/src/host-factory.ts`）用，`transport-web` 被 testkit 的传输契约测试用
（同时驱动 desktop 与 web 两条 transport）。这两个包要等 Electron 删除（M5.6）之后才能
动——不是技术阻塞，是顺序依赖。

### M6a.2 第一块：TUI crate 落地，进程树仪表盘真机验证；另两块被 §5.6 的前提错误卡住（2026-11-13）

§9.4 的选型作废后落退路 ratatui 0.30.2（见上面 M6a.1 spike）。本块把 TUI surface 建出来。

**结构与既有件复用**：`apps/desktop-tui`（lib + 薄 bin）。lib 而不是只有 bin，是为了让集成测试能反序列化 daemon 的真实产出——集成测试进不了 binary crate，而手写样本恰恰是藏契约漂移的东西。endpoint 用 `step_studio_host::lifecycle::default_endpoint()`，不自己推第二遍。连接用 `BridgeSession::connect`（`node` 为空 = 纯客户端，和 daemon 的集成测试同一条路），所以 TUI 是 daemon 的**第二个 surface**，不持有任何子进程——这正是 M2.3 复用之后才成立的事。

**read model 用类型化 struct 而不是 `Value`**：M2.5 的 P0 就是九个查询因形状变化产出 `result: null`（不是 error），于是 `.catch` 不触发、面板静默清空。类型化让漂移变成编译期或测试期失败。`tests/contract.rs` 直接调 `step_studio_host::process::read_model()` 拿 daemon 真产出反序列化——这是唯一能让契约漂移挂掉测试的办法。

**进程树仪表盘真机验证通过**：起真 daemon（fake-step 适配器），用 pty 驱动 TUI（pty 必须设 `TIOCSWINSZ`，否则 ratatui 拿到 0×0 区域、一个字符都不画——这个坑值得记），渲染出：

```
│2390770 daemon  3.8 MB │
│2390775 adapter 3.6 MB │
│2390777 cli     47.8 MB │
```

三个角色、真实 pid 与 RSS，深度按树算不按可执行文件名算。Tab 切仪表盘也通了。

**踩到并修掉的真 bug**：usage 仪表盘的「最忙时段」gauge 原本和「按模型」共用同一个 area，后渲染的胜出——于是 gauge 静默**替换**了按模型 breakdown。修成三个独立 area，测试钉住（把 gauge 改回 `body[1]`，`the_arrays_are_charted_when_they_exist` 立刻失败）。

**CJK 宽度是测试侧的坑，不是渲染侧的**：ratatui 把宽字符写进一个 cell，然后把它覆盖的后续 cell `reset()` 成空格（真实终端里被字符遮住，渲染是对的）。所以「一个 cell 一个 symbol」拼出来的文本会把 `进程树` 变成 `进 程 树`，每个 golden 比较都败在空格上而不是内容上。`dashboards::flatten()` 按显示宽度跳过被覆盖的 cell；pty 抓包时同理，`进程树` 在原始字节流里不是连续字符串。

**§5.6 的前提是错的，这卡住另外两块**。§5.6 写「四仪表盘共用 daemon query 面」，真机一跑发现：`session.agents.list` 和 `history.list` **根本不在 daemon 的查询面上**——它们在 Tauri 壳里是 shell-local（`pump.rs` 的 `SHELL_LOCAL` 表），daemon 转给适配器，适配器没有这个能力，回 `result: null`。TUI 切到这两个仪表盘时看到的是 `invalid type: null, expected struct AgentStatus`。

这一次错误是**响的**，正是类型化 read model 买来的：换成 `Value` + `.get()`，就是 M2.5 那个静默空面板。

修法明确且只有一条：store 读取逻辑抽成共享 crate（`apps/desktop-tauri/src-tauri/src/history.rs` 里读 store 的部分约 450 行：`list`/`read_session`/`store_dir`/`project_path`/`projects`/`agents`/`read_agent`/`snippet`），壳与 TUI 都调它。两份实现必然漂移，所以不能各写一份。这一块未做。

**MSRV 已抬到 1.88**（ratatui 0.30.2 的地板；0.30.0 要 1.86，同样高于原 1.85）。依赖树 131 包。

### M2.3 多面第二块：回执按 requestId 路由，不再广播给所有 surface（2026-11-13）

M2.3 的「多面」卡在一件事上：daemon 的 adapter pump 把**每一帧**扇给所有附着的 surface。于是两个 surface 会互相收到对方的回执与回合——渲染层会把别人的回合画进自己的对话流。

做法：daemon 现在按 body 里的 `requestId` 把回执送回**发问的那个** surface，其余帧（事件）照旧广播。

追代码追出的两个反直觉点，都写进了注释：

**一、关联键是 body 的 `requestId`，不是帧头的 `frameId`。** 第一版我按 `frameId` 写，查适配器时发现它每写一帧都新铸一个 id（`write_frame` → `next_frame_id()`），请求自己的 id 永远不回来。两侧皆然：TS 侧 `encodeFrame(\`request:${requestId}\`, ...)`、Rust 侧 `write_frame` 都是新铸。真正被带回的只有 body 里的 `requestId`。而事件信封 `{type, runtimeEpoch, eventSeq, stateVersion, occurredAt, event}` **不带** `requestId`——这正是按它路由安全的原因：事件永远不会被误判成回执而只送给一个 surface。

**二、requestId 原来是每会话计数器，两个 surface 会铸出同一个 id。** `shell-req-{seq}`，每个 `BridgeSession` 从 1 开始。而 daemon 把 N 个 surface 复用到**一条** adapter 连接上，协议的关联键却是每会话唯一的——这是协议与拓扑之间的真实裂缝。不修它，上面的路由会把 A 的回执送给 B，A 永久挂起（`await_reply` 自己没有超时）。改为进程唯一：`shell-{pid}-{seq}`（pid 分开两个进程——壳与 TUI 是不同进程；计数器分开同进程内的多个会话）。pid 复用要求旧进程已退出，而旧进程退出时它的 surface 一并消失、daemon 里的条目已被 `forget` 清掉，所以实际无害。

**路由表放进 lib 而不是留在 bin 里**（`crates/studio-host/src/routes.rs`，45 个单测里的 6 个是它）：它是整块复用逻辑中唯一纯逻辑的部分，纯逻辑才能不起 daemon、不起适配器就测。而且它守的失败是**静默的**——回执送错 surface，发问的那边永远等下去，日志里什么都没有。

`expect` 因此**返回被挤掉的那个 surface** 并由 daemon 打日志：两个 surface 铸了同一个 id 是协议违规，吞掉它就是把一次无法诊断的挂起写成默认行为。这一条有变异验证：把返回值改成 `None`（吞掉），`a_repeated_id_is_reported_rather_than_absorbed` 立刻失败。

**一个诚实的否定结论**：我最初写的并发端到端测试（两个 surface 同时各发一个请求）**钉不住这个 bug**——带缺陷跑它也过。原因是第二份回执 `settle` 时条目已被移除，落到广播兜底，把丢掉 slot 的那个 surface 救回来了。端到端看不到「收到了不属于自己的回执」（`drain_events` 只倒事件，回执不进公开 API），所以这条缺陷只能单测钉。测试注释已改成它真正证明的东西：两个 surface 同时各发请求都能完成（两条 relay 线程经共享 writer 写同一条 adapter 连接，帧交错会损坏适配器的流，那样没人应答、测试超时）。

既有 `two_surfaces_both_receive_the_same_broadcast` 仍然成立：事件广播这条性质没变。

### M6a.1 spike：`hqtui` 不存在，选型作废；退路 ratatui 的 headless 渲染器已实证可用（2026-11-13）

方案 §9.4 把表面 D 押在 `ports/rust` crate `hqtui` 0.2.0 上（「约 1.82 万行零依赖」fork 自管 + vendored path）。**查证结论：那个上游不存在，这一条选型无法执行。**

三条独立证据，都指向同一结论：
- crates.io API 直接查：`hqtui` / `hq-tui` / `hqtui-tui` / `qtui` 全部 HTTP 404，按名字搜索 `q=hqtui` 返回 `total: 0`。
- 仓内与 vendor 都没有：`ports/` 目录不存在于本仓，`omp-patch/vendor/oh-my-pi/crates/` 下 13 个 crate 无一匹配，全树 `find -iname "*hqtui*"` 为空。
- 全网检索（step-search）无任何以 `hqtui` 为名的项目，结果全是 ratatui / tui-rs 的中文教程。

方案自己记了它的风险（「0.2.0、无生产先例、单人维护、未上 crates.io」），也预置了退路。**取退路：ratatui 0.30.2。** 这不是新决策，是执行方案里已经写好的那一行。

退路已核实成立：crates.io 上存在，`max_version` 正是 0.30.2，`rust_version` 1.88.0——方案记的「MSRV 1.88 已录」数字准确，下载量 7065 万。

**§9.4 留的那个 spike 问题（「headless 渲染器待 M6a.1 spike 确认」，v4.2 误列为既有能力、IC-9 已改）现在有答案：退路上有，而且就是门禁机制本身。**

`ratatui::backend::TestBackend` 渲染进内存 `Buffer`，不需要终端。配合 `Buffer::with_lines([...])` 与整块 `assert_eq!`，方案要的「13 组 golden fixtures 作对拍门禁」**零额外机制即可成立**。实证（`/tmp/tui-spike`，仓库外一次性验证，未入树）：

- 40×6 画面：`Block` 标题框 + `List` 两行，无终端渲染出正确字符面；
- golden 测试通过；把期望里一格从 `daemon  1` 改成 `daemon  2` → **FAILED**，改回 → **ok**。门禁真的抓一格漂移，不是空断言。

其余 §9.4 声称的能力，逐条对退路核实：

| §9.4 对 hqtui 的能力声称 | ratatui 0.30.2 + crossterm 0.29 |
| --- | --- |
| 差分渲染 | **有**，且是内置：`ratatui-core-0.1.2/src/terminal/buffers.rs:78` 注释即「compares the current buffer with the previous buffer and passes only the changed cells」 |
| OSC 52 | **有**：crossterm 0.29 `clipboard.rs` 的 `CopyToClipboard`（base64 + osc） |
| SGR 鼠标 + Tab 焦点 | 鼠标有（crossterm 事件）；Tab 焦点框架没有，要自己写 |
| CSS Grid 风格约束布局 | 有自己的约束布局（`Layout`/`Constraint`），不是 CSS Grid |
| Braille 2×4 像素/格 + block/ASCII 模式 + 渐变 + alpha | **没有**。进程树要像素密度得手写，这是退路相对 §9.4 声称面最大的缺口 |
| 约 30 组件 | `all-widgets` 下约 25-30，量级相当 |
| 30fps（SSH 降 15） | 没有帧率控制：ratatui 只负责差分，不调速。30/15 两档要在事件循环里加 `Duration` 限速 |

代价（实测，非估算）：workspace `rust-version` 必须从 **1.85 抬到 1.88**（0.30.0 要 1.86，同样高于 1.85）——抬到 1.88 是两者中较高的那个，一步到位；依赖树 131 个包（ratatui + crossterm 及其传递依赖）；一个只画两个组件的 release 二进制 705 KB。MSRV 这一步**等依赖真正落进 workspace 时再抬**，现在抬是白付代价。

### M5.2 第一块：ed25519 签名清单（2026-11-13 完成）

`crates/studio-host/src/signing.rs`，对应 §12.4「ed25519 清单 + 公钥 bake + 版本签入防降级」。

6 个单测钉住四条各自独立的拒绝理由：签名不验（换钥匙签的同样字节）、字段被篡改
（改 digest 保留原签名）、**有效签名但版本不升**（重放旧清单——这正是版本检查必须
独立于签名检查存在的原因）、摘要不匹配。另有「非 hex 签名是 malformed 而非 bad」——
两者对读日志的人意味着不同的事：前者是打包 bug，后者是攻击。

公钥是占位符（全 0），等真发布密钥 mint 后由 build script 替换；签名密钥不在本
crate 里，签名只在发布机发生——这个分离是重点：能签名的 daemon 就等于能被逼着给任何
东西签名。

**未做**：daemon 热替换序列（排空→换包→重启）与回滚演练（GAP-2）需要真机跑升级；
NSIS 产线需要 Windows。本次只做密码学内核。

### M5.4 第一块：daemon 进程树 RSS 曲线（2026-11-13 实测）

用 `crates/studio-host/src/process.rs` 的同一套 `/proc` 遍历采样（1 Hz，60 s）：

| 项 | 实测 | 门禁（§14.6） |
|---|---|---|
| daemon 进程树合计 RSS | **56 076 kB**，60 s 内恒定 | 稳态 60 s 斜率 < 1 MB/min |
| 斜率 | **0.0 kB/min** | < 1024 kB/min |
| 峰值 | 56 076 kB（= 均值，无增长） | — |

口径：debug 构建，`--grace 600`，假 Step CLI（`fake-step.mjs`）。树含 daemon +
适配器 + node 三进程；56 MB 里大头是 node（runtime 侧），不是 daemon 本身——
daemon 单体在 M2.1 时实测 2.7 MB（release）。

**未做**：renderer/WebView 的 RSS（G-1 的 165 MB 前置条件），需要真有 WebView2 的
Windows 真机；以及 60 分钟 soak（§14.6 的 < 5 MB/h）。本次是 60 s 窗口，不是 soak。

### M3.2 进程树：阻塞点查清，并入 M2.3（2026-11-12）

上一轮记「需从零设计契约面」。本轮查清真正卡住它的：**事实在 daemon 手里，壳拿不到**。

壳用 `process_group(0)`（setsid）拉起 daemon，M2.1 验收测试已实证 daemon 在壳 `kill -9` 后同 PID 存活——即被 init 收养、不在壳的进程树里。壳侧读 `/proc` 只能看到壳与终端 PTY；daemon 是纯转发，没有非转发路径回答查询。

两条路：(a) daemon 为该 operation 终结协议，正是 M2.3 的工作；(b) 把 daemon 改回壳的子进程，直接推翻「壳重启不杀会话」——那是 M2 的存在理由。

**结论：M3.2 并入 M2.3，不是独立的 M3 项。** 此前单列是排期错误。

### M3.5 对抗评审：两条 Critical + 七处 High/Medium（2026-11-12，已修）

评审对象是刚落成的 `git.rs`。**两条 Critical 直接推翻「Git 面板全部接上」的结论**，且根因相同：模块测试全部直调函数，接线从未被验证。

1. `resolve_operation` 的 git 臂手工挑字段而非 spread → `operation`/`path`/`target`/`paths` 全丢 → 每个 `git.execute` 因缺 operation 被拒、每个 `workspace.fileTree` 列仓库根、每个 `git.diff.get` 返回空 patch。
2. `command` 传 `Want::Result` → `client.command` resolve 到无 `requestId` 的 payload → `waitReceipt(client, undefined, 60_000)` 挂一分钟。

已修，并补经 `resolve_operation` 的接线测试（`the_git_queries_carry_their_whole_input_through_resolve`）。

同批修掉：`pull` 的 match 漏 `ff-only` 臂（那是默认值也正是面板 Pull 按钮发的，每次必错）；`worktree_path` 漏 `..`（可在仓库外建 worktree）且三处 `..` 检查只 split `'/'`、漏 Windows 的 `..\..\x`；`unborn` 永不为真；`truncated` 在正好 `limit` 条时误报；`%D` 剥前缀导致本地/远端同名歧义（改完整 refname 映射）；**`revision` 从未发出使乐观并发检查在面板上全是死的**——我上一轮还在文档里写「破坏性操作一律带乐观并发检查」，那条宣称当时是假的。

第二批已修：`commit` 忽略 `paths`（加 `--only --`，否则 UI 说「提交这些文件」却把整个索引都提交了）；`discard` 对未跟踪文件直接失败（拆成 tracked→restore、untracked→移除）。

第三批已修，M3.5 收口：
- rename/copy 的类型 2 行按普通变更解析、`originalPath` 从不发出。类型 2 行多一个 `<X><score>` 字段，偏移整体后移一位，且路径对是 tab 分隔——当普通行读会得到 `path: "R100 new\told"`。已按类型 2 解析并发 `originalPath`。过程中数错一次字段偏移（`nth(4)` 应为 `nth(6)`），被自己的测试抓住。
- `state('T')`（typechange）与 `'!'`（ignored）不在 `GitFileState` 里，改落到 `modified`（参考实现同）；同时恢复 `.` 的显式 `unmodified`——我第一版把这条臂删了，只有未知码才该落到 `modified`。
- 测试失败路径不清场：两个 scratch 测试的清理写在函数末尾，`assert!` 先炸就把文件和索引都留下。改成 `Drop` guard 并提出为共享模块。**这个失败模式真的发生过**——`scratch-diff-test-1755253.txt` 曾被 `git add -A` 暂存进索引（未进提交），本次已清出。

M3.5 对抗评审闭环。

### M3.3 文件树：`workspace.fileTree` 落地（2026-11-12 完成）

渲染层按目录懒展开（`{ path: "src" }`），所以壳一次只列一层——首渲染就全走一遍会把 `node_modules` 走爆。跳过的正是让文件树变没用的那几个：`.git`/`node_modules`/`target`/`dist`/`.next`/`__pycache__`。目录优先、组内字母序。

抓到一个真 bug：越界检查写在 `trim_matches('/')` **之后**，于是 `/etc` 被 trim 成 `etc` 就看起来是相对路径、检查通过。改成先查原始值。

`workspace.open`/`pick`/`projects.list` 仍属 Host workspace registry，未接——那是 M2.3 会话目录的同一块。

### M3.4 PTY 满速最长帧（2026-11-12 完成）

§14.6 的 PTY 门禁项。用壳同款的 `portable-pty` 驱动，10 MB 持续输出，报最长帧而非均值——「均值快但每秒卡 200 ms」的终端用户是感觉得到的。

实测（release，`head -c 10M /dev/zero`，220×50）：**284.5 MB/s，2718 帧**；帧时 p50 0.012 ms / p99 0.074 ms / p99.9 0.155 ms / **max 0.988 ms**。

max 0.988 ms 远低于 §14.6 的 33 ms 帧预算，p99.9 也只有一个数量级内的余量。按同一节的口径：max 只记录不用作门禁（GC 与调度噪声让单帧超限正常），门禁统计量是 p99.9 + 连续掉帧计数。

这个数比方案要求的 10 MB/s 高出 28 倍，所以 PTY 路径不是瓶颈；瓶颈在渲染层而不在 PTY。

### M1.5 渲染侧基准：markdown 增量解析帧时（2026-11-12 完成）

§14.6 点名「markdown 增量解析帧时」是可进 CI 的渲染侧轻量基准。它每 delta 跑一次，是长回复每帧都要付的那笔钱。`scripts/bench/markdown-stream.mjs` 无头驱动 `scanStreamingMarkdown`，把上一帧结果交还（渲染器的真实用法），文档取模块自己注释里点名的昂贵形态：34427 字符、372 个 delta、含 400 行流式代码围栏。

| 口径 | p50 | p99 | p99.9 | max | 连续掉帧 |
|---|---|---|---|---|---|
| 增量（交还上一帧） | 0.003 ms | 0.022 ms | 0.227 ms | 3.270 ms | 0 |
| 每帧全量重扫（旧实现） | 0.066 ms | 0.150 ms | — | 0.342 ms | 0 |

增量比全量重扫快约 20 倍（p50 0.003 vs 0.066 ms），且**差距随文档变长而扩大**——全量重扫是 O(文档)，增量只处理新文本加固扫余量。这条基准的价值不在今天的绝对值（两者都远低于 33 ms 帧预算），而在它把「增量优化真的在起作用」变成可复测的事实：谁要是把 `previous` 交还那行删掉，p50 会立刻涨 20 倍。

按 §14.6 的门禁统计量口径：报 p50/p99/p99.9 与连续掉帧计数，max 只记录不用作门禁（GC 让单帧超限正常）。

### M2.3 会话目录第一块：`history.list` 落地（2026-11-12）

数据源是 D-1 定案时查清的那个：`~/.stepcode/agent/sessions/<project>/<session>.jsonl`，一个项目一个目录、一个会话一个文件。首行是 session 头（`cwd`/`id`/`timestamp`），其余是 entry。

**只移植渲染层要的**：TS 那份 1615 行的 reader 带着 cursor、gzip 冷归档树、512 MB 单文件上限、2 万文件扫描上限、8 条快照缓存——全是为几万会话的开发者 profile 准备的。这里一个都不满足，所以一个都不建。建的是 renderer 实际读的十个字段，最新在前。

title/summary 取会话自己的首条 user message 文本，**绝不是路径**——契约明说 summary never a path，而路径写进那个字段是泄露用户在哪工作。测试里有一条专门断言这点。

checklist 1.4 从「空」翻「齐」——M1.7 正是把它从「齐」改判成「空」的（理由是 `history.list` 被拒、区域恒渲染空态），现在原因消除了。

同批落地 `projects.list`：同一个 store，`workspaceId` 取自 session 头里的真实 `cwd` 而**不是**目录名解码——目录名是路径把 `/` 换成 `-`，而那不是单射：`step-studio` 会解成 `step/studio`。第一版我就是那么写的，还被自己一个断言正确的测试抓住（我同时在注释里写下了这个 caveat，却写了个断言相反的测试，两处都错了）。测试现改为 `assert_ne!`，专门用来阻止后人把 `project_path` 「简化」回解码。

同批落地 `home.get`：**纯聚合，没有新数据源**。authority/runtime 来自本会话的 epoch，snapshot 来自泵的 `Stream`（`spawn_pump` 多带一个参数——renderer 已经有这份字节，再走一次往返是花往返学已知的事），两个列表来自 session store。`Stream` 现在还会在 `session.state` 回来时更新快照，所以后面的 `home.get` 答的是当前状态而不是壳启动时那份。

同批落地 `workspace.open`：把项目标为激活并返回列表。**没有 history 的项目也能开**——用户刚打开的目录本来就不在 store 里，拒绝它会让「新建项目后第一次打开」这条路径直接断掉，而那正是这条命令存在的理由。

### M2.3 会话目录第二块：`workspace.pick` 落地（2026-11-13）

上一轮记「渲染层的 `pick` 入口在有 `executionTarget` 时走的是 `openDirectory()`，不走这条」，据此判定引一个 dialog 插件不值得。**这条推断是错的**，追代码追错的：`ExecutionTargetPicker.tsx:95` 的效果钩子第一行就是 `!target.id` 提前返回——`openDirectory()` 只在**远端** target 时动作，本机 target（`id === null`）下 `App.tsx:7137` 的 `if (executionTarget.id)` 不成立，直接落到 `client.command("workspace.pick")`。也就是说这条命令是本机选项目的**唯一**路径，不是「尚未有人调用」。

落地内容：`tauri-plugin-dialog` 2.8.4（连带 `tauri-plugin-fs`），`main.rs` 用 `OnceLock` 存 AppHandle（泵跑在自己线程上，为一条命令把 handle 穿透整个 bridge 不划算），`history::pick()` 开原生目录选择器后委托给已有的 `open()`——注册、置 active、返回 read model 全是 `open` 现成的，pick 只补「选哪个目录」这一段。

取消是**失败回执**而不是空列表，与参考实现一致（Electron 侧的 `WorkspacePickCancelledError`，code `UNAVAILABLE`）：渲染层靠 receipt 的 status 区分「用户关了选择器」和「没有项目」，返回 `{"workspaces":[]}` 会让新建项目流程把「按了 Escape」报成「所选文件夹未注册为项目」。为此这条命令在泵里有自己的 arm——共享本地 arm 只会发 `completed`。

`name` 入参接受但忽略：壳没有 workspace registry 可持久化显示名，行名照 `open` 的办法取目录名。带上新建项目流程里用户键入的名字需要先有那个 registry。

同批把 CI 的 Rust 门禁补上 `rust-shell` job：这条 crate 一直不在 CI 里（注释写的是「links winit，为零测试覆盖要装 X 库」），而它现在有 89 个测试，且路由表正是「名字悄悄漂错派发 arm」的重灾区——M2.5 的 P0 就是九个 shell-local 查询躺在 `ADAPTER_NATIVE` 里。补的是 GTK/WebKit 依赖 + `cargo test` + `cargo clippy -D warnings`。

**仍未移植**：`session-archive-reader` 的 cursor 分页与冷归档（为几万会话的开发者 profile 准备，这里一个都不满足）。

### M2.4 删除连锁：隐藏消费者实证，本轮不执行（2026-11-12）

规划阶段预警的 R5 已实证。`packages/runtime-stepcode/test/host-integration.test.ts:14-22` 与 `real-cli.test.ts:19` 经**相对路径** import `../../studio-host/dist/src/bridge-client.js`，两个测试**都在核心门禁名单**（`scripts/core-suites.mjs:12,15`）且当前通过。它们驱动的是 **TS 适配器回退路径**：没有 Rust 二进制的机器靠它跑。

`scripts/architecture.test.mjs` 只读 `package.json` 的 dependencies，**看不见这条相对路径**——所以删 `bridge-client.ts` 会让门禁红，而红的地方不在被删的模块上，也没有任何工具会指认它。

因此本轮不删。2507 行（`omp-usage-adapter.ts` 448 + `session-catalog.ts` 444 + `session-archive-reader.ts` 1615）的删除必须与下面这件同时发生：关掉回退路径，或把这两个测试改指 Rust 适配器。回退路径的去留属于 M5.6「Electron 删除」，在那之前删除连锁不动。

### M2.6：三路对抗评审（2026-11-12 完成，30 条 finding，24 条已修）

三路并行，各自读码不构建：并发/死锁、信任边界、数据诚实。**5 条是本 session 自己引入的**，说明 C1/C2/C3 那三个「接近零成本」的接面并不零成本。

**本 session 引入、已修**：

| finding | 后果 |
|---|---|
| `Dispatch::Local` 把 ack 当结果返回，无视 `job.want` | `capabilities.get` 交回回执形状的对象，渲染层 `usableCapabilityManifest` 读 `.capabilities.length` → TypeError，首屏后即崩 |
| `providers[].models` 缺失（`ModelProviderRecord` 必填） | composer 选择器与 `ModelConfigPage` 的 `provider.models.map` 在 `availableModels` 非空时 throw——而实测正是 2 模型 × 1 供应商 |
| `webSearch` 缺 `timeoutSeconds`/`geminiModel`/`providers`/`advanced` 四个必填成员 | Web Search 面板挂载即 throw |
| `next_frame` 对 `payload_length < 4`  panic | panic 在适配器泵线程里展开，**跳过 B1 依赖的 surface 释放清理**——壳会挂在一条静默 socket 上永不重连 |
| 全局 `to_surface` 被陈旧泵线程无条件清空 | 与已修的 `alive` 竞态同形：替换适配器已发布自己的 sender，旧泵一退就把healthy adapter 的帧丢进没人关的 socket，壳侧 `await_reply` 永久阻塞 |
| `auth.type: "none"` / `local: false` / `enabled: true` 是无据断言 | catalog 不含这些字段；`local-gateway` 很可能就是 local。已改为 `auth.type: "unknown"` 并在 `unavailableReason` 点名哪些是占位 |
| 任意 `notify` 被当成 `/usage` 面板 | `notify` 是通用呈现方法，扩展与 OAuth 登录都用它。已按方案早记录的 `extensionPath: "command:usage"` 判别 |
| 双向不变式漏了 RENAMED 表 | `session.create` / `conversation.open` 能 resolve 但未声明，`can()` 对两个壳确实服务的名字返回 false。已改为**从 RENAMED 表派生**，不再手抄第二份名单 |

**既有代码里抓到、已修**：`/tmp` 端点无属主/权限校验（目录 0700 + 属主校验 + socket 0600，任何 umask 下成立——`umask 000` 下原来会产出 world-writable socket）；bind 竞争失败者删掉赢家的 socket 文件；panic 的泵被 `join().ok()` 当成正常退出，监督者永久退光；`BridgeSession::drain_child` 跨 3 秒 sleep 持有 stream 锁；默认 `--adapter-endpoint` 不按用户隔离。

**记为缺口、本轮不修**：

1. **peer-uid 校验**。`UnixStream::peer_cred` 仍 unstable，workspace 禁 `unsafe`。因此任何能连上 socket 的本地用户都可驱动会话——这是本次评审认定「唯一实质性变差」的一条（旧实现把 socket 放在用户自己的 0755 目录里）。目录 0700 已挡住 umask 000 与路径抢占，但挡不住同 uid 的其他进程。随 M5.1 平台能力一起做。
2. **Windows `#[cfg(not(unix))]` 分支只保证编译**。其中 `serve` 臂的 `Ok(Err(Acquire::AlreadyRunning))` 与 `bind` 的真实返回类型不符，**本来根本编不过**（已顺手修）。其余待真机。
3. **`usage.get` 阻塞泵最多 30 秒**，且每次轮询都往活动会话注入一条 prompt，方案 §9.6 要求这类 prompt 不占会话轮次而适配器既未保证也未验证。正确修法是让 `/usage` 走事件回传而非占用一次 invoke，属协议形状改变，留到 M2.3。
4. ~~**HomePage 的 KPI 瓦片把空数组渲染成 "0 / 0 / 0"**~~ **已修**（`34fdbba` + 本轮）：`usage.get` 现在成功返回（空数组 + 理由 + 面板原文），而 HomePage 只在 catch 分支读 `unavailableReason`，理由永不到达。改为 `days` 为空时显示理由并附上面板原文；热力图格子本身也一并 gate 掉——全零格子同样在说「你一年没用过」。顺带修掉 `save_avatar` 不删其他扩展名导致 `load_avatar` 返回旧头像（M1.7 报的真实 bug）。

### M2.2-D1：杀壳重连 / epoch / daemon 熔断演练（2026-11-12 完成并实证）

三项演练。前两项的机制在 M2.1 / M2.2 已有测试，本次补的是**熔断演练**，而它抓出了两个叠加的 bug：

1. **`Next::Halted` 分支结尾没有 `continue`**。halt 之后执行继续往下走，又去拉起适配器——日志刷出「halted after 3 / 4 / 5 … 16」，一次演练 374 行。这是「 halt 了就停下来」在最基本的一步上没做到。
2. **Unix accept 阻塞**。`stopping` 立了也没人看见，进程挂着不退。

两者叠加的后果不是难看的日志，而是**方案 §19 要求的「守护进程无法启动」终态永远不可达**：daemon 占着端点却答不了，表面等满 20 秒 connect 超时被拒，而拉起方（壳）永远计不到一次失败，breaker 永远是 0。

修后实测：适配器起不来 → 3 次失败 → halt → 等 30 秒（给正在连接的壳一个请求重试的窗口）→ 退出并清理 socket 文件。全程 32 秒、13 行日志、退出码 0。回归测试 `a_daemon_whose_adapter_cannot_start_exits_instead_of_degrading` 钉住这条，包括「socket 文件被清掉」——留着它会让下一个 daemon 的 `acquire` 探测误判成「已有 daemon 在跑」。

### M2.2-D2：§14.2 / §14.3 门禁回填（2026-11-12 完成）

两项原写「未知，M2 回填」的数字，用 release 二进制实测（n=10，tempdir 隔离，假 Step CLI）：

| 项 | 门禁 | 实测 | 说明 |
|---|---|---|---|
| daemon connect 就绪 | < 200 ms | **p50 53 ms**（min 53 / max 53） | 从进程启动到客户端握手完成，含 daemon 拉起适配器并与之握手 |
| daemon 自身可归因段 | 同上 | **2-3 ms** | 进程启动到端点接受连接。§14.2 只对应用自身可归因段设门禁，适配器与 step 的启动明确不承诺 |
| daemon 独立 RSS | §14.3 预算 10-30 MB | **2.7 MB** | release，空闲。进程树合计 53.3 MB（3 个进程：daemon + 适配器 + node），但那是树不是 daemon |

两点值得记：① 「connect 就绪」有两种读法，差 25 倍，所以两段都测都记，门禁按严的算仍然过；② daemon 自身 2.7 MB 远低于 10 MB 预算下界——预算给它留了余量，实际用得比预算少一个数量级。

### M2.2-C3：`capabilities.get` 本地作答（2026-11-12 完成）

manifest 由派表层派生，所以问适配器要它是一次往返去学自己早就算出的东西——加 `Dispatch::Local` 由壳直接答。

**抓到的真缺口**：`capability_manifest()` 原先只遍历 `ADAPTER_NATIVE`，于是 `usage.get` / `models.get` / `capabilities.get` 这些**渲染层实际调用的名字**一个都没被声明，`can()` 对它们全返回 false——能力清单说自己不能做它能做的事。补 `FACADE_ONLY` 表（带各自诚实的 grade：两个 limited、一个 stable），双向不变式的单测同时夹住「声明的必须能 resolve」与「能 resolve 的必须已声明」。

`comparison-checklist` 5.2 翻「齐」；**5.1 仍「空」**——适配器有 `session.extensions.list` / `session.commands.list` 且 stable，但壳未把它们暴露成 facade query，渲染层拿不到。这一步只是接线，不夸大。

### M2.2-C2：模型 catalog 进 read model（2026-11-12 完成并实证）

`models.get` → 适配器 `session.models.list`（早已 stable），壳重塑成 `ModelConfigReadModel`。

**真 step CLI 实测**：1.0 秒回传 2 个模型 × 1 供应商（`local-gateway`），带 selector / reasoning / contextWindow / maxTokens。composer 的模型选择器因此可用。

`ModelConfigReadModel` 有 17 个必填字段，其中角色、预设、循环顺序、回退链、登录供应商、webSearch 路由承载的都是 OMP 多供应商机制，Step RPC 一个都不暴露。所以 `availableModels` / `providers` / `modelProviderOrder` 填真数据（providers 按 distinct provider 去重，不按模型条数），其余留空并带 `unavailableReason` 点名缺什么。`omp-models-adapter.ts` 那 3018 行**不移植**——约 95% 是多供应商 OAuth/probe/webSearch，Step 单供应商下是死码。

### M2.2-C1：`/usage` 面板经桥抵达（2026-11-12 完成并实证）

`usage.get` → 适配器 `usage.panel`：把 `/usage` 当 prompt 发过去，等 fire-and-forget 的 `notify`，壳重塑成 `TokenUsageReadModel`。

**真 step CLI 实测**：1.5 秒回传 6194 字符面板，`usage.panel` 回 `completed`。面板含热力图（7 行 × 46 周列）+ "Activity insights"（总 chats / 活跃天 / 总 token）。

**关键实测约束**：stdin 必须全程不关。关掉它 CLI 就取消活动扫描，只回 `extension_error`（`Activity scan cancelled`）——方案 §9.6 早提醒过，本次两次撞到后才按它保持 `StepRpc` 的进程存活。另外 `--mode rpc` 的 prompt 信封是 `{"id","type":"prompt","message"}`，裸写 `/usage` 会被当 JSON 解析而报 `Unrecognized token '/'`。

**1.3 / 6.2 仍是「空」，原因是数据不在**：热力图格子只有强度分档，没有按小时/按模型/精确按日的 token 数。把分档填进 `totalTokens` 就是编数字，所以结构化数组留空 + `unavailableReason` 说明缺什么，面板原文经契约新增的 `panel?: string` 原样送达。详见 `comparison-checklist.md` 的用量条。

### M2.2：壳侧重连 + 全量刷新（2026-11-12 完成并实证）

适配器崩溃重启后，M2.1 会把壳留在一条断掉的 relay 上且没有任何恢复路径。本条补上。

| 层 | 改动 |
|---|---|
| `studio-bridge` | 新增 `BridgeSession::close_reason()`：区分「这条命令失败」与「已经没有可问的人」 |
| 泵 | 每 50 ms tick 检查会话是否已断，断了就退出并把原因交出去；`spawn_pump` 因此多返回一个 `JoinHandle`。接管 session 的**第一帧**是 `runtime.changed` 宣告——重连不产生命令也不产生适配器事件，渲染层没有别的途径知道权威变了 |
| 壳 | `SessionState` 改为 `Arc<SessionInner>` 句柄，监督者能在 Tauri 命令持有 `State` 期间换掉 session；`supervise()` 带界退避（250 ms → 5 s 上限）永不放弃地重连；`Backoff` 是纯状态机，有单测 |
| 渲染层 | `load()` 在权威变更时重跑，键同时用 epoch 与 runtimeId |

**四个非显然的点**：

1. **daemon 必须主动关掉 surface 的 socket**。适配器死后它的泵线程退出，但 slot 里的 sender 克隆还活着 → surface 的写线程永远等不到 channel 关闭 → socket 两边都开着 → 壳的会话不死、泵永远不知道要重连。这是 B1 真正的断点，而且不在重连算法里。
2. **等「活着的适配器」而不是「有适配器」**。槽里会留着刚死那个直到替换者起来，新表面拿到死 handle 会被立刻释放成 broken pipe。
3. **存活标记收进 `AdapterHandle`**，不做独立槽。两个字段会让表面看到「新适配器 + 旧死亡标记」的竞态，误杀一条好连接（实测抓到）。
4. **宣告事件从泵内部发**，用 stream 自己的 cursor。从泵外发会和第一个真事件撞号，reducer 的 `cursorRelation` 判 duplicate 后把它吃掉。

**真实验收**：跑壳 → 自检回合完成 → `kill -9` 适配器 → daemon 记 `the adapter ended; releasing the surface` → 壳记 `session ended: adapter closed the connection; reconnecting` → `session 2 established` → 新适配器起来（pid 变更）。

**测试**：daemon 端到端从 5 个增至 6 个，新增「适配器死后表面被释放且 daemon 继续服务」；壳侧 Rust 测试 26 → 28（`Backoff` 两条 + `install` 换代 + 宣告两条）。workspace clippy `-D warnings` 零警告，`npm run check` 全绿。

### M2.1：host daemon 化——生命周期 + 监督者（2026-11-12 完成并实证）

新增 `crates/studio-host`（workspace 成员），壳从「spawn 适配器 + 当客户端 + 退出时杀适配器」三合一改为**纯客户端**。

| 层 | 文件 | 内容 |
|---|---|---|
| 监督者 | `src/supervisor.rs` | 退避 + 熔断 + 宽限的纯状态机。时间是「自监督者创建的单调偏移」而非 `Instant`，所以整套策略是被告知内容的纯函数，测试不需要 sleep 也没有时钟 trait |
| 持久化 | `src/state.rs` | daemon epoch（单调、持久化）+ daemon 熔断器（5 次 / 10 分钟窗口）。写入用 `create_new` + `rename`，崩溃在写的瞬间只留旧文件或新文件 |
| 端点 | `src/lifecycle.rs` | 单例探测（连得上就是活的，连不上才清陈旧 socket 文件）、`bind` 前建父目录、排空时钟 |
| 转发 | `src/relay.rs` | 字节级帧转发。不解码是承重墙：重新编码就得决定盖哪个 epoch |
| 适配器 | `src/adapter.rs` | daemon 拥有 child；握手失败时杀 child（`Child::drop` 不杀进程，这是孤儿化的直接来源） |
| daemon | `src/bin/studio-host.rs` | 单例 → mint epoch → 监督线程 + 排空看护线程 + 平台形状 accept 循环 |

**监督参数**（`supervisor.rs` 顶部一个 `Policy`）：宽限 60 s（复用方案里同义的热窗口）、初始退避 500 ms、倍率 2、上限 30 s、3 次失败即停、窗口 600 s。存活过宽限的子进程视为健康重启，立即拉起且不清预算。没有抖动——单个被监督子进程形不成雷惊群，且 `std` 没有 RNG；M2.3 扇出到 N 个适配器时才是加 full jitter 的时刻。

**真机验收（Xvfb，`STEP_STUDIO_SELF_CHECK=1`，假 Step CLI）**：

1. 跑壳 → 壳 detached 拉起 daemon（`process_group(0)`，stdio 重定向到 `.step-studio/daemon.log`）→ daemon spawn 适配器 → 适配器 spawn `step`；
2. 自检回合完整：prompt accepted、`conversation.message.started/completed`、tool started/completed、逐字 delta `he`/`llo`/`!`、终态回执 `completed`；
3. `kill -9` 壳 → **daemon / 适配器 / step 同 PID 全部存活**（这是 M2 的核心主张）；
4. 重启壳 → 连上同一个 daemon 与同一个适配器（PID 未变，没有新起适配器），回合再次正常完成。

**四个只有真机或无头测试能抓的问题**（都不是 review 能看出来的）：

| 问题 | 症状 | 根因 |
|---|---|---|
| 适配器 `serve()` 是无限循环 | daemon 被杀后适配器不死，`step` 被 reparent 握着 `~/.stepcode` 会话——静默数据损坏（R1） | 连接结束只是线程退出，进程继续 `accept`。加 `STEP_STUDIO_EXIT_ON_DISCONNECT`，M2 拓扑下 daemon 是适配器唯一客户 |
| `BridgeSession` reader 线程持有 socket 克隆 fd | `drop(session)` 后对端看不到 EOF，宽限时钟永远等下去；第二个表面永远拿不到 relay | `Child` 被 drop 不杀进程，同理 fd 被克隆不随 session 消失。`Drop` 里半关闭（`shutdown(Write)`） |
| `/tmp/step-studio-<uid>/` 目录不存在 | daemon `bind` 直接 ENOENT，壳等 30 秒后报「daemon 没起来」 | `UnixListener::bind` 要求父目录存在，而短路径约束恰好要求它落在没人建的临时子目录里 |
| `--grace` 没接到 `GraceClock` | 排空时钟跑的是 600 秒而不是传入的 3 秒 | `main` 里写死了 `GraceClock::new(DRAIN_GRACE)` 常量，没用 `config.grace` |

另有一条死锁是**设计级**的：`wait_for_adapter` 在轮询期间持有适配器槽锁，`serve_surface` 的「等适配器」循环永远拿不到锁；`serve_surface` 又把锁持有整个中继期，堵住排空路径。拆成 `writer` 走 `Arc<Mutex<>>`、槽只用于取一个 `AdapterView` 视图后两条路都通。

**测试**：`cargo test --workspace` 91 个通过，clippy 零警告，`cargo fmt --check` 干净，`npm run check` 全绿。其中 daemon 端到端 5 个：壳重启保住会话、杀 daemon 不留孤儿适配器、无表面则排空退出、第二个 daemon 让位给在跑的那个、`runtime.shutdown` 真的排在飞回合。

**明确跳过**（理由记在 §19）：信号处理排空（fd 级联已覆盖正确性，代价是 `unsafe` 或新依赖，收益只是 5 秒优雅窗口）、OS 自动拉起（M5.2 与 NSIS 产线一起）、daemon↔surface 新协议与多表面（M2.2/M2.3）。

**CI 覆盖**：`step-studio-host` 已同时进 `rust` 与 `rust-windows` 两个 job。Windows 那一项是必要的而不是凑数——它的 `#[cfg(not(unix))]` 分支（管道名端点、`announce_departure` 空实现、bind 前不建目录）没有任何其他途径被编译。本地无 Windows 真机，这些分支**未经运行验证**，只保证可编译。

**环境备注**：本机 AF_UNIX socketpair 缓冲只有约 250-300 字节（`/tmp/buf_probe2` 在 cargo 之外同样复现），所以「先写完再读」的测试写法在这里必然阻塞；socket 测试必须让写线程与读并发。

### M1.2c-terminal：`ompStudioTerminal` 真 PTY（2026-11-12 完成）

底部 Terminal 接上真 PTY（`portable-pty` 0.9），不是行缓冲——AGENTS.md 明确要求
「Terminal 两种模式都用本机 Shell」。壳级 `$SHELL`，TERM 设为 `xterm-256color`，
输出由独立 reader 线程读，经 `omp-studio:terminal:data` / `:exit` 两个 Tauri 事件
通道送出，JS 侧单 listen 扇出到 Set（与 desktop sender 同一个理由：Tauri 的 `listen`
返回 Promise，逐订阅注册会让早于 resolve 的 unsubscribe 找不到目标）。

实现上三个决定：

1. **清理靠 `Drop`，不靠显式 shutdown**。最初写了个 `shutdown()` 函数，然后发现没人调——
   死代码警告逼我面对一个问题：显式调用要在每条退出路径都记得，忘一次就留下孤儿 shell
   占着 workspace。改成 `Terminal` 的 `Drop` kill child，Tauri 关闭时自动销毁托管状态，
   覆盖包括 panic 在内的所有路径。
2. **PTY 核心从 Tauri 命令里拆出来**。`open_shell(id, cols, rows)` 不碰 AppHandle、不碰
   状态，于是真行为可测——见下面那个集成测试。
3. **`write`/`resize`/`dispose` 同样拆成自由函数**，命令只是 Tauri 边界。

**真 PTY 集成测试**（不是 mock）：开真 PTY、起真 shell、写
`printf 'PONYTAIL_OK\n'` + `exit`、读回输出、断言含 `PONYTAIL_OK`，再 drop 句柄。
另一个测试证明 `take_writer` 之后 master 仍可 `resize`（portable-pty 的 `take_writer`
内部克隆 fd，这点值得钉住，否则将来有人「优化」成 move 就会静默破坏 resize）。

新增 `test/terminal-bridge.test.ts`（5 个用例）：扇出的同步性与身份精确性、JS 与 Rust
双向命令名对齐、两个事件通道名逐字节相同、recording 三件套保持缺席。

未接的部分，与 chrome 同一条边界：`recordingStart/Stop/Status` 要 Host 的 artifact
library 才能产出 `artifactId`，留空，renderer 禁用录音按钮。`workspaceId` 接收但忽略——
把 workspace id 映射到目录需要 Host 的 workspace registry，终端因此起在壳自己的工作目录。
这是相对 omp-studio 的真实缺口，记录下来而不是糊过去。

**UI 侧未能实机验证**，原因明确且不在本桥：`TerminalPane` 的 `canCreate` 要求
execution target 处于 connected（`TerminalPane.tsx:43`），而当前没有 workspace 打开
（Host 未接），所以不会自动建终端。这不是终端桥的缺陷，是同一个 Host 缺口的另一面。
另外过程里发现两个自己的操作错误：`pkill -f step-studio-desktop-tauri` 会匹配到
Bash 工具自己的命令行从而自杀（改用 `pkill -f "step-studio-deskto[p]-tauri"`）；
以及 Xvfb 捕获图是 1400x900 而窗口只占 1280x800，按捕获图坐标点窗口外的黑区会全部落空。

### M1.2c-chrome：`ompStudioChrome` 壳级子集（2026-11-12 完成）

`vite-env.d.ts:51-290` 声明了 139 个 chrome 条目。切开看：后半是自动更新器
（打包面，不是产品面），前半大多数是 Host 背后——artifacts、媒体上传、prediction、
browser observation、service definitions。**这次只做壳自己那部分**：平台标识、开链接、
系统通知、操作者头像、两个 perf 上报。共 10 个条目，每个都有 `apps/renderer/src` 里的
真实调用点。

边界不是待办清单，是一条真实的线：壳不跑 Host，所以 Host 背后的方法留空，renderer 按
既有可选调用画诚实不可用态（与 M1.1 三桥缺失时同一条路径）。

实现上三个值得记的点：

1. **`platform` 从 UA 推，不发命令**。它是同步只读字段，而 WebView 引擎恰好固定了答案：
   WebView2 → Windows、WKWebView → macOS、WebKitGTK → Linux。Electron preload 是从
   `process.platform` 钉同一个值。
2. **`setTheme` 与 `setConversationViewState` 是刻意 no-op**。前者在 Electron 里染的是
   Windows 原生标题栏覆盖层，纯 Tauri 下整个标题栏就是 renderer 自己画的 web 内容，
   已经自己换主题；后者是 Electron 给「隐藏的 webview」对应的会话做后台节流用的，单
   WebView 壳既没有 Host 可节流也没有隐藏画面。两个都写了注释说明为什么空转是对的，
   而不是留一个待实现的坑。
3. **字节跨界用普通数组**。Tauri IPC 确实能传 `ArrayBuffer`，但依赖它会让桥的契约取决于
   当前走哪条传输；`Vec<u8>` 的含义永远是 JSON 数字数组，`Uint8Array` 在 JS 侧重建。

新增 `test/chrome-bridge.test.ts`（6 个用例）做**跨语言反漂移**：把 `chrome-bridge.ts` 里
所有 `tauriInvoke("name")` 与 `chrome.rs` 里所有 `#[tauri::command]` 双向对齐——改名字、
加死命令、漏接线，三种都会红。这条和已有的 `command-table.test.ts` 守的是同一类问题：
编译过、类型过，只在运行时以一句含糊的 Tauri 错误暴露。

实机验证：Xvfb 下 renderer 的 perf reporter 打进来了，stderr 出现
`[chrome] perf sample role=renderer ...`；转写、模型选择器、会话跨重启续接都正常
（截图里是上一次 self-check 的对话被 `--continue` 捞回来）。

### M1.2b：query / command 接真实会话 + 事件泵（2026-11-11 完成）

`query` 与 `command` 从 stub 改为走 `src-tauri/src/pump.rs`：一个独占 `BridgeSession` 的
线程，把 renderer 的 facade 词表翻译成适配器的 operation kind，并把适配器事件包成
`ClientEvent` 送回。**Xvfb 下真机跑完一整个回合**：用户消息与助手回复都渲染进转写，
流式逐字到达，回执在回合结束后落地。

自检输出（`STEP_STUDIO_SELF_CHECK=1`，stderr 逐条打印）：

```
[self-check] prompt accepted
[self-check] #1  conversation.message.started
[self-check] #2  conversation.message.completed
[self-check] #3  conversation.message.started
[self-check] #4  conversation.message.delta The
...
[self-check] #11 conversation.message.delta ONG
[self-check] #12 conversation.message.completed
[self-check] receipt core.prompt -> completed
[self-check] #13 conversation.turn.completed
[self-check] receipt conversation.open -> completed
```

设计与实现上的六个决定，每条都由一个具体的约束逼出来：

1. **泵独占会话，不用 `Mutex<BridgeSession>`**。事件是广播不是寻址，所以一个会话上同时只能有
   一个消费事件的在飞命令；mutex 挡不住两条 Tauri 命令同时 await 两个回执、把一路流劈成两半。
   所有操作走同一个线程，这条不变量是结构性的，也正因为如此 `core.abort` 能在回合流式期间
   照样打到会话——这是 mutex + 计数器方案给不了的。
2. **`core.prompt` 是唯一的 `Dispatch::Turn`**。适配器对它立即回 `accepted`，终态回执要等回合
   结束（`main.ts:603-611`）。等它就会把泵占住整个回合，abort 永远进不来，所以它走
   `send_command` 直接放行，终态回执由 `conversation.turn.completed` 反推合成——适配器在那个
   事件里不报 requestId，配对关系只能由泵自己记。其余操作都是秒回终态，走 `invoke_with`，
   期间到达的事件由同一个回调转发，模型切换不会吞掉流式。
3. **`conversation.changed` 的 `update` 原样透传**。适配器事件的 payload 本来就是
   `ConversationRuntimeEvent`，不需要翻译；桥只补信封与 `streamSeq`。这是 M1.2b 工作量比预期小
   一个数量级的原因——原先以为要移植 `studio-host` 的事件投影，实测不需要。
4. **能力清单由派发表派生**。renderer 的 `can()`（`App.tsx:3831`）对未声明的能力一律返回 false，
   而空清单会让 composer 永远禁用。清单若手写就会和 `resolve_operation` 漂移，且漂移是静默的
   （界面给一个壳必然拒绝的动作）。所以它由同一张表生成，并有单测双向夹住：声明的必须能
   resolve，能 resolve 的必须已声明。
5. **bootstrap 必须带 `snapshot`**。`reduceBootstrap` 把 `bootstrap.snapshot` 放进
   `entities.snapshot`，缺了它 `snapshotReady` 永远是 false（`App.tsx:4205`），composer 不启用。
   这一项编译过、单测过，只有看界面才发现。
6. **不移植 Host**。Electron 主进程组合了约 160 个操作，背后是 git / archive / residents /
   models / workspace 一整套服务（`apps/desktop/src/host-composition.ts`，1556 行）。这些都不在
   Tauri 壳里，所以 `resolve_operation` 之外的名字一律返回 `CAPABILITY_UNAVAILABLE`，renderer
   画诚实空态。复用整个 Host 而不是移植它是另一个决定，已记入方案 §19 决策日志。

途中修掉的与新增的原语：

- **`BridgeSession::drain_events()`**（`crates/studio-bridge/src/client.rs`）。事件只在有命令
  await 时被取走；两条命令之间没人取，回合的事件会一直堆在 inbox 里直到下一条命令碰巧取走。
  泵空闲时（`recv_timeout` 50ms）必须自己清空队列。
- **`EventSink` 是可注入的闭包**，不是 `WebviewWindow`。这样泵能在没有窗口的情况下被驱动——
  `STEP_STUDIO_SELF_CHECK` 就是靠它在无显示、无法操作 composer 的机器上证明整条回合链路。
  顺带让泵可测。
- **状态版本不随 delta 前进**。适配器只对改变状态的事件递增 `stateVersion`（`main.ts:162-167`），
  `runtime-projection.ts:38-50` 记了为什么每个 token 重发一次未变的 read model 是成本不是功能。
  镜像这个行为，renderer 的 `advanceConnection` 取最大值才不会失真。
- **`iso_utc`**：`std` 没有民用日期格式化，workspace 也没有 time crate。内联了 Hinnant 的
  `civil_from_days`（每个日期库都在用的同一个算法），配四个已知时刻的单测，含闰年与世纪边界。

自检之外，UI 侧证据：Output 面板显示 `conversation.open` 完成；模型选择器解析出
`step-3.7-flash`；转写里用户消息与助手回复都在。

已知上限（`ponytail:` 注释同级，如实记）：

- **prompt 失败没有终态回执**。适配器的 `core.prompt` catch 只发 `failedReceipt`，那不是事件，
  泵看不到；只有 `turn.completed` / `turn.aborted` 会合成回执。所以模型侧失败会让 composer
  停在「运行中」。升级路径：给适配器加一个 turn 失败事件，或让泵对 `core.prompt` 也 await 回执
  （代价是放弃 abort 能在回合中进入）。用户可手动 abort。
- **不产 `snapshot` 事件**。`entities.snapshot` 停留在 bootstrap 时的那份，`isStreaming` 等状态
  在回合中不刷新。选这个方向是因为反方向（snapshot 里 stateVersion 比 bootstrap 旧）会被
  `reduceSnapshot` 当陈旧丢弃，需要额外对齐。升级路径：回合结束时读一次 `session.state` 再发
  `snapshot` 事件。
- ~~GTK 菜单栏（File / Edit / View / Help）还在~~ **此条判断错误，已纠正**：那条栏不是原生菜单，
  是 renderer 自己画的 `TitleMenu`（`App.tsx:8701-8732`，应用自定义标题栏的一部分）。
  Tauri 与 tao 都不建 GTK 菜单栏（已在两者源码里查证），所以它属于要复刻的 UI，不该删。

### 续接是怎么接上的

Step 按 project 存 session，所以壳传给 agent 的工作目录**必须跨启动稳定**——
最初用的是 per-PID 临时目录，每次启动都是新 project，`--continue` 永远找不到上一个
session。现在用 repo 根目录。

适配器通过 `STEP_STUDIO_SESSION=continue` 选择性开启；桌面 Host spawn 时不带这个变量，
行为不变。

## Windows 交叉编译（2026-11-11 打通）

从这台 Linux 服务器产出 Windows exe 已验证可行。命令：

```bash
cargo xwin build --release -p step-studio-desktop-tauri --target x86_64-pc-windows-msvc
# 产物：target/x86_64-pc-windows-msvc/release/step-studio-desktop-tauri.exe（约 17 MB）
```

四个前提，缺一不可，都是这次实测踩出来的：

1. **`cargo install cargo-xwin --locked`**——它会首次构建时自动下载 MSVC CRT 与 Windows SDK
   （约 1.1 GB，落在 `~/.cache/cargo-xwin`）。
2. **必须禁掉本机 IPv6**：`sudo sysctl -w net.ipv6.conf.all.disable_ipv6=1`（default 同）。
   原因是 `download.visualstudio.microsoft.com` 的 DNS 同时返回 A 与 AAAA，而本机 IPv6 是
   黑洞路由——SYN 发出去无人应答也无人拒绝。cargo-xwin 的 ureq 会因此**永久挂住**
   （表现为连接 ESTAB 到 CDN、0% CPU、缓存目录不增长）。`curl -4` 正常、`curl -6` 秒失败，
   就是这个症状。禁用后 connect 立即返回错误，自动回落 IPv4。
3. **`llvm-rc` 要在 PATH 里叫这个名字**：系统只有 `llvm-rc-18`，而 `tauri-winres` 找
   `llvm-rc`。`sudo ln -sf /usr/bin/llvm-rc-18 /usr/local/bin/llvm-rc`。缺了它 build
   script 直接 panic：`NotAttempted("llvm-rc")`。
4. **clang + lld-link**（本机已有 clang 18）。

**分发不需要安装包**：Tauri 的 release exe 是自包含的（renderer dist 在编译期压进二进制，
与平台无关），把 exe 拷到 Windows 机器即可直接运行。安装程序（setup.exe / msi）只是体验优化，
不是分发前提。

未验证项：`tauri build` 的 npm CLI 链路（NSIS 安装包）尚未跑通——需要让 `tauri build` 走
xwin 的 clang-cl + lld-link 链接器，并把 `~/.cache/cargo-xwin` 里 MSVC CRT 的 include/lib
路径配成持久化（cargo-xwin 自己是给子进程设环境变量，不写 `.cargo/config.toml`）。
`makensis` 已装好（v3.09）。这条不影响 exe 产出，只影响"有没有安装向导"。

exe 在目标机运行时需要 WebView2 Runtime（Windows 11 自带，Win10 需安装一次）。

**尚未在 Windows 上运行验证**：exe 编译链接通过，但 init script 能否绕过 WebView2 的
`script-src`、自定义协议在 WebView2 上提供内嵌资产、bridge 注入时序——这三条只在
WebKitGTK 上实证过。要真正确认需要一台 Windows 机器或 CI。

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
| `crates/studio-host/` | 新增：M2.1 常驻 host daemon（bin + supervisor + lifecycle + state + relay） |
| `crates/studio-bridge/src/client.rs` | 改：`spawn_adapter` 提为公共函数（daemon 与壳共用同一份 argv）；`Drop` 改为按 child 所有权门控的 `shutdown()` + 半关闭 |
| `crates/step-studio-runtime/src/bridge_server.rs` | 改：`runtime.shutdown` 从假排空改为有界真排空（5 s）；`serve_one` 返回 JoinHandle；`exit_on_disconnect` 时连接结束即进程结束 |
| `crates/step-studio-runtime/src/config.rs` | 改：读 `STEP_STUDIO_EXIT_ON_DISCONNECT` |
| `apps/desktop-tauri/src-tauri/src/bridge.rs` | 改：`connect_session` 从「spawn 适配器」改为「连 daemon，连不上就 detached 拉起再连」；client-only session；epoch 来自 daemon |
| `apps/desktop/src/host-composition.ts` | 改：`STUDIO_RUNTIME_EXECUTABLE` 选择 custom runtime 源，并让显式指定的 limited 运行时获得 session controller |
| `apps/desktop-tauri/` | 新增：Tauri v2 主壳（M1.1 脚手架 + bridge 注入 + 六个命令 stub） |
| `packages/transport-desktop/src/ipc-validation.ts` | 新增：`createDesktopIpcBridge` 等从 `apps/desktop/src/` 迁入，桥本身零改动 |
| `apps/desktop/src/preload.ts` | 改：桥工厂三件套改为从 `@omp-studio/transport-desktop` 导入 |
| `scripts/architecture.test.mjs` | 改：`transport-desktop` 允许依赖 `client`；`sourceFiles` 跳过 `target`；electron 豁免按路径而非 key 判定 |
| `scripts/core-suites.mjs` | 改：登记 desktop-tauri 两个测试 |
| `package-lock.json` | 修：补回缺失的 `packages/runtime-stepcode` 条目（此前任何 `npm install` 都会失败） |
| `package-lock.json` | 改：登记 `@step-studio/runtime-stepcode` workspace 链接 |
| 其余 | 与上游一致 |

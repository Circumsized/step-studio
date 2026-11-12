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
4. **HomePage 的 KPI 瓦片把空数组渲染成 "0 / 0 / 0"**。`days` 恒空（数据不在），瓦片却无条件显示年度总量/近 7 天均值/峰值，而 CLI 面板里的真实总量被丢掉。与「分档扮 token 数」同源，只是从另一个方向到达。属渲染层改动。

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

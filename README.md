<p align="center">
  <img src="icon.png" width="96" height="96" alt="Step Studio">
</p>

<h1 align="center">Step Studio</h1>

<p align="center">
  <strong>Step Code 的 <code>step</code> CLI 的桌面控制台</strong><br>
  类型化 Studio Bridge · Tauri (Rust) 壳 · Windows / macOS
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT"></a>
  <a href="CHANGELOG.md"><img src="https://img.shields.io/badge/version-0.1.17-informational.svg" alt="0.1.17"></a>
  <a href="docs/getting-started.md"><img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS-0078D4.svg" alt="Windows | macOS"></a>
  <a href="package.json"><img src="https://img.shields.io/badge/node-%3E%3D22-339933.svg" alt="Node 22+"></a>
</p>

---

Step Studio 把 [Step Code](https://github.com/can1357/oh-my-pi) 的 `step` CLI 搬到桌面窗口里。
会话、审批、Agent Hub、Skills、模型、Git、终端都通过一条**类型化的 Studio Bridge** 驱动，
不解析 TUI 文本、不啃 ANSI 输出、不模拟按键——控制面是语义 command，不是屏幕抓取。

整条链路是 Rust：Tauri 壳拉起 `step-studio-host`（daemon），daemon 拉起
`step-studio-runtime`（adapter），adapter 通过 JSONL stdio 与 `step --mode rpc` 对话。

<p align="center">
  <img src="docs/images/workbench.png" alt="Step Studio 工作台" width="920">
</p>

## 链路

```
step-studio-desktop-tauri.exe    Tauri 壳（Rust）+ React renderer
  └─ step-studio-host.exe        daemon：监听、鉴权、生命周期、adapter 监管
       └─ step-studio-runtime.exe adapter：把 Studio command 翻译成 step 的 JSONL
            └─ step              Step Code CLI（--mode rpc）
```

三个可执行文件**必须在同一个目录**：壳先在自己同目录找 daemon，daemon 用同样的顺序找 adapter。
装机版天然满足；手工拷贝时漏一个，壳会打一行 `[bridge] no step-studio-host binary found` 然后什么都不做。

adapter 从 `PATH` 找 `step`，也可以用 `STEP_STUDIO_STEP_CMD` 指向别处。
**没有它，daemon 和 adapter 照样起、HMAC 握手照样完成**，只有真正调 CLI 的那一步失败。

## 快速开始

需要 Node.js 22+ 与 Rust。

```powershell
git clone --recurse-submodules https://github.com/Circumsized/step-studio.git
cd step-studio
npm install
cd apps\desktop-tauri
npm run tauri:dev
```

`tauri:dev` 先构建 preload 桥与 renderer，再打开桌面窗口，首次需要几分钟。
要的是可分发的安装包时，把最后一行换成 `npm run tauri:build`。

Windows 上从 Linux 交叉构建要加 `--target x86_64-pc-windows-msvc --runner cargo-xwin`，
完整前提与踩过的坑在 [`apps/desktop-tauri/src-tauri/BUNDLE-TARGETS.md`](apps/desktop-tauri/src-tauri/BUNDLE-TARGETS.md)。

拿到 Windows 上要跑什么、抓什么，见 [`docs/windows-real-machine-test.md`](docs/windows-real-machine-test.md)。

## 能做什么

- **对话工作台** — Composer（芯片、`@` 提及、slash、图片）、工具卡片、会话变更、子代理、Plan / Vibe / 审批。
- **会话与项目** — 本地工作区注册表、历史、归档；Renderer 只见到不透明 id，看不到本机绝对路径。
- **Agent Hub / Skills / 模型** — 库存与配置走 Host；没有 read model 的表面保持禁用，不用假数据填满。
- **本机能力** — Git、文件树、终端 PTY、系统通知、浏览器打开；控制面是语义 command，不是 shell 脚本拼装。
- **托管 Runtime** — Rust daemon + 适配 `step --mode rpc` 的 adapter；工件走 Ed25519 校验。
- **SSH 工作区（开发预览）** — 在项目/分支栏选择本机或远端工作；兼容性与未验收边界见 [SSH 使用说明](docs/ssh-workspaces.md)。

<p align="center">
  <img src="docs/images/home.png" alt="首页" width="920">
  <img src="docs/images/hub.png" alt="Agent Hub" width="920">
  <img src="docs/images/models.png" alt="模型与供应商" width="920">
</p>

## 仓库结构

```
apps/desktop-tauri/   Tauri 壳（Rust）+ preload 桥；安装包配置也在这里
apps/desktop-tui/     终端界面，与桌面壳共用同一套 store 读取逻辑
apps/renderer/        Vite + React 工作台
packages/             11 个 @step-studio/* 包：协议、client、transport、平台、测试工具包
crates/               Rust：studio-bridge（帧与握手）、studio-host（daemon）、
                      step-studio-runtime（adapter）、step-studio-sessions、step-studio-peer-uid
omp-patch/            遗留层：oh-my-pi 上游子模块 + overlay + 接缝补丁（活的 runtime 是 step）
docs/                 使用与开发文档
```

## 协议不变量

类型与散文冲突时，以 `packages/studio-protocol` 与 `client-contract` 为准。

- Renderer 不得拿到 Bridge token、进程句柄或 Step 会话路径。
- 未知 mutation 失败关闭；`accepted` 不等于成功。
- Runtime 丢失会封旧 epoch，未完成的 accepted 变为 `outcome_unknown`。
- 握手是双向的：challenge 由验证方选，双方都要证明自己持有 token。

## 当前状态

诚实起见，说清楚什么验过、什么没有：

| 项 | 状态 |
| --- | --- |
| Windows exe 与 NSIS 安装包 | **已产出**：`Step Studio_0.1.0_x64-setup.exe`，含壳 + daemon + adapter 三个文件 |
| 跨平台构建 | Linux 主机可交叉构建 Windows（`cargo-xwin`）；主机目标另可出 `.deb` |
| 全量门禁 | `npm run check`、`cargo test --workspace`（除一项门禁外诊断测试）、`cargo clippy` 全绿 |
| Windows 实机运行 | **未验证**——壳能否起 WebView2、命名管道握手、adapter 起 CLI，都要一台 Windows 机器 |
| `msi` 安装包 | 产不出（Linux 上 Tauri 直接跳过），要 Windows 主机或 wine |
| WebView2 | Windows 11 自带；Windows 10 需装一次 Evergreen Bootstrapper |

## 文档

| 文档 | 内容 |
| --- | --- |
| [docs/getting-started.md](docs/getting-started.md) | 从源码运行与构建 |
| [docs/development.md](docs/development.md) | 开发循环、预览模式 |
| [docs/architecture.md](docs/architecture.md) | 包职责与协议不变量 |
| [docs/windows-real-machine-test.md](docs/windows-real-machine-test.md) | Windows 实机测试操作手册 |
| [AGENTS.md](AGENTS.md) | 协作者工作约定 |
| [CHANGELOG.md](CHANGELOG.md) | 面向用户的变更 |

## 贡献

见 [CONTRIBUTING.md](CONTRIBUTING.md)。唯一的检查入口是 `npm run check`，按改动范围选切片：

```bash
npm run check -- --fast          # 架构约束 + 选择器规则 + 编译
npm run check -- --only renderer # 只编译渲染层依赖闭包，再跑核心测试
npm run check                    # 架构 + 类型 + 核心测试
```

## 许可证

[MIT](LICENSE)。第三方声明见 [NOTICE](NOTICE)。

`omp-patch/` 保留对上游 [oh-my-pi](https://github.com/can1357/oh-my-pi) 的 overlay 与接缝补丁，
其版权与许可记录在 [NOTICE](NOTICE)；本仓库其余部分是独立工作。

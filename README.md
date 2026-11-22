<p align="center">
  <img src="icon.png" width="96" height="96" alt="Step Studio">
</p>

<h1 align="center">Step Studio</h1>

<p align="center">
  <strong>step CLI（Step Code）的桌面控制台</strong><br>
  类型化 Studio Bridge · Tauri 工作台 · Windows / macOS
</p>

<p align="center">
  <a href="README.en.md">English</a>
  ·
  <a href="docs/README.md">文档</a>
  ·
  <a href="CHANGELOG.md">更新日志</a>
  ·
  <a href="https://github.com/can1357/oh-my-pi">oh-my-pi</a>
</p>

<p align="center">
  <a href="https://github.com/Circumsized/step-studio/actions/workflows/ci.yml"><img src="https://github.com/Circumsized/step-studio/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT"></a>
  <a href="CHANGELOG.md"><img src="https://img.shields.io/badge/version-0.1.17-informational.svg" alt="0.1.17"></a>
  <a href="docs/getting-started.md"><img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS-0078D4.svg" alt="Windows | macOS"></a>
  <a href="package.json"><img src="https://img.shields.io/badge/node-%3E%3D22-339933.svg" alt="Node 22+"></a>
  <a href="https://linux.do/"><img src="https://img.shields.io/badge/LINUX%20DO-Community-FFB003.svg" alt="LINUX DO"></a>
</p>

Step Studio 是 Step Code 的 `step` CLI 的桌面壳。它把会话、审批、Agent Hub 和工作区接到类型化的 Studio Bridge 上，而不是去解析 TUI 文本、ANSI 或按键宏。

<p align="center">
  <img src="docs/images/workbench.png" alt="Step Studio 工作台：新建对话欢迎页、活动热力图与 Composer" width="920">
</p>

桌面壳为 Tauri（Rust）+ React renderer，Runtime 侧是 Rust daemon 与适配 `step --mode rpc` 的 adapter。查看 [更新日志](CHANGELOG.md)。

## 工作台亮点

同一扇窗口里把 Skills、用量、供应商、能力和 Git 收齐，不用在 TUI 和一堆网页之间来回跳。

### 1. 快速选择的 Skills 侧栏

侧栏一键打开技能与插件抽屉，按项目 / 全局 / 内置分组。点「加入」就能把 skill 放进当前草稿，对话里的 `/skill:` 胶囊也能回看刚用过的技能。

<p align="center">
  <img src="docs/images/skills.png" alt="Skills 侧栏：按项目、全局、内置分组，一键加入草稿" width="920">
</p>

### 2. 好看的数据查看与统计

首页有按年 / 月 / 周 / 日切换的 Token 折线与热力图。对话顶栏随时展开本轮用量、缓存命中、TPS 和子 Agent 花费，Agent Hub 里每个子代理也有自己的数字。

<p align="center">
  <img src="docs/images/home.png" alt="首页 Token 折线与热力图" width="432">
  &nbsp;
  <img src="docs/images/telemetry.png" alt="对话顶栏 Token 用量详情" width="432">
</p>

### 3. 统一供应商管理

模型配置页把 Anthropic、OpenAI、OpenRouter、本地 LM Studio 等供应商收在一处，开关、端点和 `modelProviderOrder` 同页完成。角色页给 `@default` / `@plan` / `@task` 指定模型和思考强度。

<p align="center">
  <img src="docs/images/models.png" alt="模型配置：统一供应商列表" width="432">
  &nbsp;
  <img src="docs/images/roles.png" alt="角色配置：按任务指定模型" width="432">
</p>

### 4. 能力中心管理

Skills、Plugins、MCP、Slash Commands 同一页开关、探测和打开目录。Agent Hub 盯着主会话和子 Agent 的状态、花费与上下文占用。

<p align="center">
  <img src="docs/images/capabilities.png" alt="能力中心：Skills / Plugins / MCP / Slash" width="432">
  &nbsp;
  <img src="docs/images/hub.png" alt="Agent Hub：子代理列表与用量详情" width="432">
</p>

### 5. Git 内嵌便捷工具

右侧 Git 面板直接暂存、看 diff、写 commit，Fetch / Pull / Push 和提交图都在对话旁边，不用离开工作台。

<p align="center">
  <img src="docs/images/git.png" alt="工作台右侧 Git 面板：diff、commit 与提交图" width="920">
</p>

> [!TIP]
> **安装与运行：** 前往 [GitHub Releases](https://github.com/Circumsized/step-studio/releases) 下载 Windows x64 / ARM64 安装包或 macOS Apple Silicon DMG，或参考下方说明从源码构建运行。macOS 包为 ad hoc 签名，首次打开可能需要在「系统设置 → 隐私与安全性」确认。如遇问题欢迎提交 [Issue](https://github.com/Circumsized/step-studio/issues)。
>
> **macOS（Apple Silicon，macOS 13+）：** 发布附带 `Step-Studio-<版本>-macos-arm64.dmg` 时，打开 dmg，把 Step Studio 拖进“应用程序”。目前是 ad hoc 签名、尚未公证的预览版：首次打开会被拦截，请到“系统设置 › 隐私与安全性”点“仍要打开”，或执行 `xattr -dr com.apple.quarantine "/Applications/Step Studio.app"`。也可以按 [docs/getting-started.md](docs/getting-started.md) 从源码构建。

## 能做什么

- **对话工作台** — Composer（芯片、`@` 提及、slash、图片）、工具卡片、会话变更、子代理、Plan / Vibe / 审批。
- **会话与项目** — 本地工作区注册表、历史、归档；Renderer 只见到不透明 id，看不到本机绝对路径。
- **Agent Hub / Skills / 模型** — 库存与配置走 Host；没有 read model 的表面保持禁用，不用假数据填满。
- **本机能力** — Git、文件树、终端 PTY、系统通知、浏览器打开；控制面仍是语义 command，不是 shell 脚本拼装。
- **托管 Runtime** — Rust daemon + 适配 `step --mode rpc` 的 adapter；工件走 Ed25519 校验。
- **SSH 工作区（开发预览）** — 在项目/分支栏选择本机或远端工作，支持系统密钥和账密连接的源码实现；兼容性与未验收边界见 [SSH 使用说明](docs/ssh-workspaces.md)。

## 快速开始

需要 Node.js 22+。完整步骤见 [docs/getting-started.md](docs/getting-started.md)。

```powershell
git clone --recurse-submodules https://github.com/Circumsized/step-studio.git
cd step-studio
npm install
cd apps\desktop-tauri
npm run tauri:dev
```

`tauri:dev` 先构建 preload 桥与 renderer，再打开桌面窗口，首次需要几分钟。
要的是可分发的安装包时，把最后一行换成 `npm run tauri:build`；拿到 Windows 上要跑什么、
抓什么，见 [docs/windows-real-machine-test.md](docs/windows-real-machine-test.md)。

壳起来后会自己拉 daemon（`step-studio-host`），daemon 再拉 adapter
（`step-studio-runtime`）——三个可执行文件必须在同一个目录，装机版天然满足。
adapter 从 `PATH` 找 `step` CLI，也可以用 `STEP_STUDIO_STEP_CMD` 指向别处；
没有它，daemon 与 adapter 照样起、握手照样完成，只有真正调 CLI 的那一步失败。
模型与登录沿用 Step Code 自己的配置。本仓库不内置任何供应商密钥。

`npm run omp:*` 那一套（install-deps / overlay:apply / keys / build:host）是
`omp-patch/` 遗留层的构建路径，与上面的 step 链路无关，见
[omp-patch/README.md](omp-patch/README.md)。

## 仓库结构

```
apps/desktop-tauri      Tauri 壳（Rust）+ preload 桥
apps/desktop-tui       终端界面
apps/renderer          Vite + React 工作台
packages/             协议、Host、客户端、传输、平台
omp-patch/            遗留层：oh-my-pi 上游子模块 + overlay + 接缝补丁（当前 runtime 走 step）
crates/               Rust：studio-bridge / studio-host / step-studio-runtime 等
packaging/            发布签名密钥与说明
ui_reference/ver1     视觉参考，不是产品代码
docs/                 使用与开发文档
```

改某一块功能时，从 [doc/feature-index.md](doc/feature-index.md) 跳到文件，不要全库盲搜。

## 架构

```
Renderer → StudioClient → Desktop IPC → Host facade
                                    ├─ 本机：会话目录 / Git / 工作区
                                    └─ Bridge → step-studio-runtime → step --mode rpc
```

不变量（详见 [docs/architecture.md](docs/architecture.md)）：

- 类型与散文冲突时，以 `packages/studio-protocol` 与 `client-contract` 为准。
- Renderer 不得拿到 Bridge token、进程句柄或 Step 会话路径。
- 未知 mutation 失败关闭；`accepted` 不等于成功。
- Runtime 丢失会封旧 epoch，未完成的 accepted 变为 `outcome_unknown`。

## 文档

|                                                    |                    |
| -------------------------------------------------- | ------------------ |
| [docs/getting-started.md](docs/getting-started.md) | 从源码运行              |
| [docs/development.md](docs/development.md)         | 开发循环、预览模式、补丁 regen |
| [docs/architecture.md](docs/architecture.md)       | 包职责与不变量            |
| [docs/releasing.md](docs/releasing.md)             | 版本、tag、安装器         |
| [CONTRIBUTING.md](CONTRIBUTING.md)                 | 如何提 PR             |
| [SECURITY.md](SECURITY.md)                         | 漏洞披露               |
| [CHANGELOG.md](CHANGELOG.md)                       | 面向用户的变更            |
| [omp-patch/README.md](omp-patch/README.md)         | overlay / 接缝补丁     |

## 贡献

欢迎 Issue 与 PR。请先读 [CONTRIBUTING.md](CONTRIBUTING.md) 与 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)。

不要把安全问题发到公开 Issue。安全报告走 [SECURITY.md](SECURITY.md)。

中文交流、建议与反馈欢迎来 [LINUX DO](https://linux.do/) 社区，感谢佬友们的支持。

上游 oh-my-pi 本身的缺陷请提到 [can1357/oh-my-pi](https://github.com/can1357/oh-my-pi)；本仓库只在 `omp-patch/` 保留对它的 overlay 与接缝补丁，其余部分（Tauri 壳、Rust daemon 与 adapter、renderer、协议包）是独立工作。

## 许可证

[MIT](LICENSE)。第三方声明见 [NOTICE](NOTICE)。

钉住的 Runtime 来自 [oh-my-pi](https://github.com/can1357/oh-my-pi)（MIT；Pi by Mario Zechner，omp by Can Bölük）。Studio 的 overlay 与接缝补丁是本仓库的独立工作，同样以 MIT 授权。

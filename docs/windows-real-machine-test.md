# Windows 实机测试

把产物弄到一台 Windows 机器上、跑起来、抓证据的操作手册。

**要测量什么、为什么测量**在 [`design/review/plan-2026-11-15.md`](../design/review/plan-2026-11-15.md) §22 B 节，这里不重复。
这份只讲：带什么、怎么跑、日志在哪、抓什么、怎么回报。

## 1. 带过去的东西

从 `target/x86_64-pc-windows-msvc/release/` 把这三个拷到同一个目录：

| 文件 | 大小 | 作用 |
| --- | --- | --- |
| `step-studio-desktop-tauri.exe` | 21.2 MB | 壳，双击启动 |
| `step-studio-host.exe` | 780 KB | daemon，由壳拉起来 |
| `step-studio-runtime.exe` | 882 KB | adapter，由 daemon 拉起来 |

或者只带安装器 `bundle/nsis/Step Studio_0.1.0_x64-setup.exe`（12.2 MB，未 Authenticode 签名，SmartScreen 会拦）。装完三个文件都在同一个 `$INSTDIR` 下。

可选，不参与上面的链路：`step-studio-tui.exe`（1.2 MB）、`pty-bench.exe`（344 KB）。

体积是 2026-11-21 全量重建后的实测值，重新构建会变；核对文件时以目录里的实际大小为准。

**三个文件必须在同一个目录。** 壳先在自己同目录找 `step-studio-host`，再找 `<repo>/target/{release,debug}/step-studio-host`；daemon 用同样的顺序找 adapter（`bridge.rs:644`、`studio-host.rs:381`）。装机版三者同处 `$INSTDIR`，所以安装这条路天然成立；手工拷文件时漏一个，壳会打一行 `[bridge] no step-studio-host binary found` 然后什么都不做。

## 2. 前置条件

- Windows 10 / 11，x64。
- **WebView2 Runtime**：Windows 11 自带；Windows 10 需要装一次 Evergreen Bootstrapper。缺它壳起不了窗口，失败点在 WebView2 初始化，不是我们的代码。
- **`step` CLI（可选，但决定能测多深）**：adapter 默认从 `PATH` 找 `step`，用 `STEP_STUDIO_STEP_CMD` 可以指向别处（`step-studio-runtime/src/config.rs:68`）。**没有它，daemon 和 adapter 照样起、握手照样完成**，只有真正调 CLI 的那一步失败。§22 B 的前 4 项测量都不需要它。

## 3. 跑起来

在放着三个 exe 的目录里双击 `step-studio-desktop-tauri.exe`，然后确认三个进程都在：

```powershell
Get-Process step-studio-desktop-tauri, step-studio-host, step-studio-runtime
```

三个路径各有用处：

| 看什么 | 在哪 |
| --- | --- |
| daemon 日志 | `<cwd>\.step-studio\daemon.log`（`bridge.rs:606`）。`cwd` 是壳进程的当前目录，从资源管理器双击就是 exe 所在目录；用 `STEP_STUDIO_CWD` 可以改 |
| 桥 token | `%LOCALAPPDATA%\Step Studio\daemon\token`（`state.rs:176` + `state_dir_under`） |
| 命名管道 | `\\.\pipe\step-studio-<hash>`，`<hash>` 是 `USERNAME` 的 FNV-1a（`lifecycle.rs:55`） |

本次的管道名跑这一句就知道：

```powershell
[System.IO.Directory]::GetFiles("\\.\pipe\") | Select-String step-studio
```

## 4. 抓什么

按 §22 B 的顺序逐条做，每条给**命令 + 完整原始输出**，不要只给结论。

有一条现在就能预告，测第 3 项时会直接看到：`.step-studio` 目录和 token 的加固**都只写在 `#[cfg(unix)]` 分支里**。
`prepare_dir_at`（`state.rs:154`）在 Windows 上只做 `create_dir_all`，没有 `0700`、没有属主校验；`write_token_at`（`state.rs:196`）的 `0600` 同理。
`bridge.rs:272` 那段 `.step-studio` 的加固也是 `#[cfg(unix)]`。所以「token 是 0600」这个前提在 Windows 上**尚未实现**，不是待验证的假设——第 3 项要测定的是它在默认 ACL 下实际是什么。

## 5. 怎么回报

每条测量一个独立小节：命令、完整原始输出、一句话结论。

结论和文档冲突时，以原始输出为准，并把冲突写进 §22。
那里已经有两条推论在等这一份输出收口：DACL 对 Everyone 只读（文档链推断，未实测）、标识级别模拟后能读到真实客户端 SID（两位独立检索都停在「没有哪页明说」）。
这两条各有一页文档依赖它们，实测值反过来就都错。

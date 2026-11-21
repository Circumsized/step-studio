# 为什么 `bundle.targets` 在三个平台文件里

`tauri.conf.json` 的 `bundle.targets` 是**空列表**，每个平台在自己的
`tauri.<platform>.conf.json` 里声明要打什么包。Tauri 会自动把平台配置与基础配置合并。

## 为什么不写在基础配置里

基础配置里的扁平列表会点名**只存在于一个 OS** 的 target，而 `tauri build` 遇到
当前平台没有的 target 是**直接报错**，不是跳过。所以：

| 文件 | targets |
| --- | --- |
| `tauri.windows.conf.json` | `nsis`、`msi` |
| `tauri.macos.conf.json` | `dmg`、`app` |
| `tauri.linux.conf.json` | `appimage`、`deb` |

Linux 那两个不是发布目标（方案只写 Windows 与 macOS），但它是唯一一个不需要
Windows 机器、也不需要签名身份就能产出并检查安装包的平台。

## 一个会重复踩的坑

**这些文件必须是严格 JSON，不能有注释。**

Tauri 有两条解析路径，行为不一致：Node 侧的 CLI（`tauri info`、`tauri build`
的前半段）按 JSON5 解析，`//` 注释能过；而 Rust 侧的 `tauri-build` build script
按严格 JSON 解析，注释会让 `cargo build` 失败：

```
unable to parse JSON Tauri config file at .../tauri.linux.conf.json
because expected value at line 1 column 1
```

`npm run check` 抓不到这个——它只跑 TypeScript 编译，不编译 Rust。只有
`cargo build` / `cargo test` 会。所以改完这三个文件要跑一次 Rust 门禁，
不能只跑 `npm run check`。

## 怎么真的打出一个安装包

```
cd apps/desktop-tauri
tauri build --target x86_64-pc-windows-msvc --runner cargo-xwin
```

三个参数都不是可选的：

- **必须 `cd apps/desktop-tauri`**。`beforeBuildCommand` 的工作目录是调用 `tauri`
  的目录，不是 `src-tauri`。在仓库根跑，`npm run build:bridge` 解析到根
  `package.json`，直接 `Missing script`。
- **必须 `--runner cargo-xwin`**。默认 runner 是 `cargo`，交叉目标下没有
  `link.exe`，链接阶段失败。
- **`--target` 要与 resources 路径里的三元组一致**，否则 Tauri 找不到要打包的文件。

## 为什么不能直接 `cargo build --workspace` 再单独 bundle

`bundle.resources` 指向的是 `target/` 里的构建产物，而 `tauri-build` 的 build
script 在**编译期**校验这些路径存在。干净的 `target/` 下直接跑
`cargo-xwin build --release --workspace` 会失败：

```
resource path `../../../target/x86_64-pc-windows-msvc/release/step-studio-runtime.exe` doesn't exist
```

原因是 `step-studio-desktop-tauri` 编译时，同一次 cargo 调用里的
`step-studio-host` / `step-studio-runtime` 二进制还没产出——壳只依赖它们的 lib，
cargo 不构建依赖包的 bin。四个不依赖 `tauri-build` 的 exe 会正常出来，只有壳失败。

`tauri build` 本身没有这个问题：它的 `beforeBuildCommand` 在壳编译**之前**跑，
里面那句 `cargo build -p step-studio-host -p step-studio-runtime` 正好把 sibling
先建好。所以在 Windows 主机上 `tauri build` 一条命令到底。

Linux 交叉编译不行——`beforeBuildCommand` 里是裸 `cargo`，没有 `link.exe`。
要分两步，先让 sibling 存在，再 bundle 时把 beforeBuildCommand 覆盖掉：

```bash
cargo-xwin build --release --target x86_64-pc-windows-msvc \
  -p step-studio-host -p step-studio-runtime -p step-studio-tui -p pty-bench
cargo-xwin build --release --target x86_64-pc-windows-msvc -p step-studio-desktop-tauri

cd apps/desktop-tauri
tauri build --target x86_64-pc-windows-msvc --runner cargo-xwin -b nsis \
  -c '{"build":{"beforeBuildCommand":"npm run build:bridge && npm run build -w @step-studio/renderer"}}'
```

## `bundle.resources` 要写成映射，不能写成数组

安装器要把 `step-studio-host.exe` 和 `step-studio-runtime.exe` 放到壳的同目录
（`daemon_binary` / `native_runtime_command` 只在那里找）。数组形式会保留相对
`src-tauri` 的路径结构，`../../../` 变成 NSIS 的 `_up_`，装到 `$INSTDIR` 上方三层：

```nsis
File /a "/oname=_up_\_up_\_up_\target\x86_64-pc-windows-msvc\release\step-studio-host.exe"
```

映射形式（源 → 目标文件名）才落到 `$INSTDIR` 根下：

```json
"resources": {
  "../../../target/x86_64-pc-windows-msvc/release/step-studio-host.exe": "step-studio-host.exe"
}
```

## 还没有接上的部分

配置写好了，安装器也产出过了（`Step Studio_0.1.0_x64-setup.exe`，11.66 MiB），
但**产线仍没接**：没有任何 CI 调用 `tauri build`。`scripts/release-assets.mjs` 算出的
`Step-Studio-Setup-<版本>-windows-<arch>.exe` 指向的路径与捆绑器实际输出的
`Step Studio_<版本>_x64-setup.exe` 不是同一个，发布前要对一遍。

Windows 侧还缺 WebView2 引导与 VC++ 运行时——NSIS 不自带这两个（M5.3）。
`msi` 在 Linux 上产不出（Tauri 直接跳过），要 Windows 主机或 wine。

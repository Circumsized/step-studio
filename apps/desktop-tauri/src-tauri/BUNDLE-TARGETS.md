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

## 还没有接上的部分

配置写好了，但**产线没接**：没有任何 CI 或脚本调用 `tauri build`，安装器从未被
产出过。`scripts/release-assets.mjs` 算出的
`OMP-Studio-Setup-<版本>-windows-<arch>.exe` 一直指向一个不存在的文件。

Windows 侧还缺 WebView2 引导与 VC++ 运行时——NSIS 不自带这两个（M5.3）。

# M2.3 `workspace.pick`：为什么没做，以及它背后更值得修的一条

## 契约要什么

`workspace.pick` = 开系统目录选择器 → 注册所选目录 → 返回列表，新的那项
`active: true`。取消时抛 `WORKSPACE_PICK_CANCELLED`（`App.tsx:4033-4036` 认这个码）。

## 没做的原因，以及它不只是「缺个插件」

渲染层的入口 `pickProject`（`App.tsx:7136-7141`）有两条路：

```ts
if (executionTarget.id) { go("workbench"); executionTarget.openDirectory(); return; }
const handle = await client.command("workspace.pick", {});
```

第二条路需要原生目录对话框。Tauri 侧**没有任何目录选择 API**——
`chrome-bridge.ts` 的 `tauriInvoke` 共 11 个，全是 avatar/theme/url/notify/perf，
没有 `pick_directory` 之类；`chrome.rs` 的 9 个 `#[tauri::command]` 同样没有。

但第一条路的问题更值得记：`executionTarget.openDirectory()` 是 **Electron 独有的
API**——`apps/desktop/src/chrome-logs.ts:31` 声明、`chrome-updates.ts:671` 与
`host-factory.ts:197` 用 `properties: ["openDirectory"]` 走 `dialog.showOpenDialog`。
Tauri transport 没有实现它，所以 `pickProject` 的两条路在 Tauri 壳上**都是死的**。

也就是说：不是「`workspace.pick` 缺一个插件所以那条入口不可用」，而是
**「选择项目」这个入口在 Tauri 壳上整体没接**，而渲染层看不出区别——它照发
`workspace.pick`，收到 `CAPABILITY_UNAVAILABLE`，显示「创建项目失败」。

## 处置

`workspace.pick` 保持拒绝（`SHELL_ANSWERED` 里没有它，`resolve_operation` 返回
`None` → `IpcError::unavailable` → 渲染层显示失败文案）。**不引 dialog 插件**：
本机没有 Windows/macOS 可验，为一个入口引一个跨平台行为差异很大的插件，不值得。

真正该做的是把 `openDirectory` 接到 Tauri transport 上（`tauri-plugin-dialog` 的
`open({ directory: true })`），那是渲染层**已经在调**的路径，比 `workspace.pick`
更早该通。这一条记在这里，等 M4.4 评审结论一起排期。


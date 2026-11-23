# 兼容性策略

四个版本号，各自独立演进，不匹配时会发生什么。这份文档是唯一事实源；
代码里的常量如果和这里不一致，**以代码为准并改这里**。

## 四个版本号

| 版本 | 定义处 | 当前值 | 约束什么 |
| --- | --- | --- | --- |
| **Studio 协议版本** | `packages/studio-protocol/src/contracts/protocol.ts` `STUDIO_PROTOCOL_VERSION` | `1` | 帧格式：大端长度前缀、五键帧头、记录/字段分隔符。**改它等于换一种线上方言** |
| **客户端契约版本** | 同文件 `CLIENT_CONTRACT_VERSION`（`client-contract` 转出口） | `2` | `ClientBootstrap` 的形状。renderer 与 host 必须一致 |
| **Runtime 兼容窗口** | 更新清单里 `studioProtocol: { min, max }`（`packages/runtime-installer/src/update-index.ts`） | 由 Runtime 侧声明 | 这个 Runtime 能驱动哪些协议版本的壳 |
| **应用版本** | `package.json` `0.1.17`；`apps/desktop-tauri/package.json` 与 `tauri.conf.json` 仍是 `0.1.0` | 不一致 | 更新清单的 `minAppVersion` 比对它 |

最后一行是**已知的不一致，不是疏漏**：壳作为独立 surface 从 `0.1.0` 起版，而仓库根版本跟随 workspace。改哪一边都牵动发布口径，记在这里而不是各自猜。

## 谁兼容谁

```
renderer ──契约版本──> client-contract (2)
                              │
host ────协议版本(1)──> studio-protocol
                              │
                    step-studio-runtime ──JSONL──> step CLI
                              │
                    studioProtocol.min/max ← Runtime 在更新清单里声明
```

- **renderer ↔ host**：契约版本必须**精确相等**。不等则 renderer 拒绝 bootstrap，见下。
- **host ↔ runtime**：协议版本必须**精确相等**。帧头校验不匹配即拒帧。
- **壳 ↔ Runtime**：走 `studioProtocol: { min, max }` **窗口**，不是相等。这是唯一允许跨版本共存的一对，因为 Runtime 可以独立升级（stable / canary 两个通道）。

## 不匹配时发生什么

### renderer 拿到的契约版本不对

`packages/client/src/studio-client.ts`：

```ts
if (bootstrap.contractVersion !== CLIENT_CONTRACT_VERSION) {
  throw { code: "UNAVAILABLE", message: `Client contract mismatch: ...` };
}
```

**这是精确相等判断，没有窗口。** 也就是说：契约版本从 2 升到 3 的那一天，旧壳对一切新 host 都是 `UNAVAILABLE`，没有降级路径。

用户看到的是这句英文技术文案，没有「该升级谁、去哪升级」。

### host 与 runtime 的协议版本不对

帧头校验失败，连接在建连阶段断开。用户看到的是连接失败，不是版本提示。

### 壳比 Runtime 的 `minAppVersion` 旧

更新清单在下载前就会拒绝，见 `runtime-installer` 的 `minAppVersion` 断言。

## 支持承诺

当前是 `0.1.x` 公开预览，**承诺 N-1**：

- 壳支持清单里 `studioProtocol.max` 等于自己协议版本的 Runtime，以及 `max` 大一级的 Runtime（向前一级）。
- Runtime 支持 `min ≤ 协议版本 ≤ max` 的壳。
- 契约版本**不承诺**跨版本：升到 3 时，2 的壳与 3 的 host 不互通，需要同批发布。

这条承诺目前的执行情况：**只有协议版本有窗口机制，契约版本没有**。契约版本的不匹配是硬失败。要把 N-1 真正做到契约层，需要给 `ClientBootstrap` 也加一个适配层——那是设计工作，不是配置工作。

## 弃用流程

1. 在 `packages/studio-protocol/src/contracts/` 里给要弃用的字段标 `@deprecated`，写清替代物与移除版本。
2. 保持**一个**次要版本的读取兼容：旧字段仍然解析，但不生成。
3. 在 `CHANGELOG.md` 的对应条目里列出弃用，因为用户是从那里看到破坏性变更的。
4. 移除时同时删读取分支和测试——留着的读取分支会被下一个人当成仍然有效的契约。

## 未验证

- **兼容窗口的真实行为没有实机验过**：`studioProtocol.min/max` 的判定逻辑只在单测里覆盖，没有跑过「旧壳 + 新 Runtime」这个组合。
- **诊断页不显示这四个版本号**。版本不匹配时用户无从判断差在哪，只能看到一句 `UNAVAILABLE`。这是 [#11](https://github.com/Circumsized/step-studio/issues/11) 里明确要做而没做的部分。
- **应用版本的三处不一致**（`package.json` / `tauri.conf.json` / `desktop-tauri/package.json`）没有对发布口径的影响评估。

# Step Studio 签名密钥

本目录只放**发布签名用的 Ed25519 公钥**与多密钥信任表，不放私钥。

| 文件 | 内容 |
| --- | --- |
| `step-studio-release-2026a.pem`、`step-studio-release-2026b.pem` | SPKI 格式公钥，`2026b` 是当前活跃密钥 |
| `trusted-keys.json` | `activeKeyId` + `keyId → 文件名` 映射，验签时按 keyId 选钥 |

轮换密钥用 `npm run release-keys`（`scripts/release-keys.mjs`）：生成新密钥对、把公钥写到这里、
更新 `trusted-keys.json`。私钥不进版本库，由发布流程在受控环境使用。

## 谁读这些密钥

| 消费方 | 用途 |
| --- | --- |
| `scripts/build-update-assets-v2.mjs` | 构建更新工件时按 `trusted-keys.json` 载入公钥 |
| `scripts/verify-remote-environment.mjs` | 验签远端环境包 |
| `scripts/publish-update-release.mjs` | 发布前复验候选签名与远端环境签名 |
| `packages/runtime-installer/src/update-manifest.ts`、`remote-environment.ts` | 客户端侧的签名域分隔前缀与验签 |

签名域分隔前缀是 `step-studio-update-v2` 与 `step-studio-remote-environment-v1`，
keyId 形如 `step-studio-release-2026a`。**这四类字符串必须同批修改**：只改验签侧不改签发侧
（或反之）会让验签失败；全改则此前签发的工件全部失效。`outputs/` 目前不存在，没有已签发工件。

## 打包不在这里

安装包由 Tauri 的捆绑器产出，配置在
[`apps/desktop-tauri/src-tauri/tauri.windows.conf.json`](../apps/desktop-tauri/src-tauri/tauri.windows.conf.json)，
完整前提与踩过的坑写在
[`apps/desktop-tauri/src-tauri/BUNDLE-TARGETS.md`](../apps/desktop-tauri/src-tauri/BUNDLE-TARGETS.md)。

```powershell
cd apps\desktop-tauri
npm run tauri:build
```

`npm run pack:win` / `pack:mac` 一族在 `package.json` 里有声明，但指向的 `.mjs` 文件不在本仓库，
跑起来立即失败；那一套是 electron-builder 时代的打包线，随 Electron 壳一起废弃了。

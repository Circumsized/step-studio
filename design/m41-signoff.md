# M4.1 六页面像素签收 — 仪器修好后的首轮结果（2026-11-14）

> 状态：🟡 仪器已修好并产出 10 张互异截图；逐模块像素判定仍要人眼（B3）。
> 本轮真正完成的是**把「不能比」变成「能比」**，并在这个过程中发现签收仪器本身一直是坏的。

## 本轮最重要的发现：签收仪器从来没工作过

`scripts/capture-page-signoff.mjs` 声称「一次跑出全部页面」。实际上它跑出的
**10 张字节完全相同**的图：

```
$ md5sum <before>/*.png | awk '{print $1}' | sort -u | wc -l
1
```

原因链：

1. harness 入口（`preview-app-harness-entry.tsx`）读 `?route=`，算出 `qaRoute`，
   但只在 `preserveSettings !== "1"` 时才写 `omp.lastRoute`；
2. 而 `capture-page-signoff.mjs` **一直带着 `preserveSettings=1`**；
3. 于是 route 被算出来然后丢掉，`appSettings` 也不写，`startupPage` 落到默认值
   `"workbench"`（`appSettings.ts:78`）；
4. 十次导航全部落在 workbench，产出十个文件名、一张图。

没有任何报错。脚本最后还打印「Next: compare each against omp-studio by eye」——
一份看起来像证据的输出。**这比没有签收更糟**：它会被当成已经看过。

已修两处，并加了不会再次静默的护栏：

- harness：`omp.lastRoute` 改为无条件写。`preserveSettings` 的本意是「保留人自己选的
  主题/密度/语言」，不该顺手把 route 也保留掉。
- 脚本：去掉 `preserveSettings=1`（签收仪器要的是每次一致的确定性设置，不是保留偏好），
  并对每张截图做 sha256 去重——任何一张与别张相同即打印 `DUPLICATE of <page>` 并以退出码
  4 结束。

## 分辨率口径已统一

参考图 `ui_reference/ver2/__shots/` 24 张**全部是 1658×846**。此前捕获是 1280×800，
`m41-capture.md` 记的「差 1.7 倍」由此而来——不先统一口径，逐模块比对无从下手。
现在视口就是 1658×846，捕获与参考同尺寸。

## XWD → PNG 的桥（`scripts/xwd2png.py`）

`m41-capture.md` 说「本机没有 ImageMagick，XWD 无法转 PNG，所以逐模块比对做不了」。
这一条现在不成立：XWD 的格式全部写在头部，不需要任何库。

```
$ uv run --with pillow python scripts/xwd2png.py capture.xwd out.png
capture.xwd: 1280x800 -> out.png
```

两个踩过的坑，都写在脚本注释里：`XWDColor` 是 **12 字节**不是 16（错了不会报错，
只会让像素整体错位、图像斜切）；`ncolors` 而不是 `colormap_entries` 决定颜色表长度
（TrueColor 显示也会写它自己用不到的 colormap）。

对 2026-11-12 那份 `design/m41-workbench.xwd` 的转换结果是：**一张「Rendererer error」
崩溃页**。也就是说 M4.1 此前连一张可用的捕获都没有。

## 本轮产出

`design/signoff/*.zh.png`，10 张，1658×846，互异（md5 去重 = 10）：

| 文件 | 页面 |
| --- | --- |
| `home.zh.png` | 首页（工作台） |
| `history.zh.png` | 会话历史 |
| `settings.zh.png` | 设置 |
| `diagnostics.zh.png` | 诊断中心 |
| `capabilities.zh.png` | 能力中心 |
| `model-config.zh.png` | 模型配置 |
| `agent-hub.zh.png` | Agent Hub |
| `media.zh.png` | 媒体 |
| `evaluation.zh.png` | 评测 |
| `workbench.zh.png` | 工作台 |

复现：起 dev server（`cd apps/renderer && npx vite --port 5179`）后
`node scripts/capture-page-signoff.mjs --out design/signoff`。

## 首轮逐页比对（三态：齐 / 空 / 缺）

只记我能从图里直接读出的。**逐模块像素判定仍然要人眼**——下面是结构比对，不是像素比对。

### 设置页：`settings.zh.png` ↔ `ui_reference/ver2/__shots/light-settings-perms.png`

| 比对点 | 态 |
| --- | --- |
| 左分组导航 + 右表单的主体版式 | **齐**（同一模式：左侧分组列表，右侧该组的表单行） |
| 顶栏 | **缺**：参考是「pi 图标 + OMP Studio + 搜索 + 返回工作台 + 设置面包屑 + 主题切换」；我们是「文件/编辑/视图/帮助 菜单栏 + 页面页签轨」。不是同一套 chrome |
| 左侧 IDE 侧栏（项目与对话 + EXPLORER 文件树） | **缺**：参考有，我们没有。我们的左栏是设置自己的分组导航 |
| 设置分组 Taxonomy | **缺**：参考 General / Models and Providers / Permissions / Sessions / Preview / Advanced（6 组）；我们 常规/背景/对话与交互/权限与安全/上下文与记忆/文件与终端/任务与执行/高级/运行偏好（10 组） |
| 默认落地的分组 | **缺**：参考落在 Permissions；我们落在 常规 |
| Permissions 内容（Review / Workspace / Full Access 三卡 + 粒度控制开关列表） | **未比**：参考图是 Permissions 页，我们截图落在 常规。要判定就得按 `?settingsGroup=` 再抓一张 |

结论：**版式范式齐，信息架构缺**。这不是「数据没接」，是产品 IA 不同——按
`comparison-checklist.md` 的规矩，「缺」要么补实现，要么在方案里记一条决策说明为什么不做。
目前方案里没有这条决策。

### 诊断页：`diagnostics.zh.png` ↔ `ui_reference/ver2/__shots/light-diagnostics.png`

| 比对点 | 态 |
| --- | --- |
| 页签 + 卡片式检测列表 | **齐**（运行环境 / 会话与维护 两个页签，下面是卡片与检测行） |
| 更新提示条 | **齐**（「有可用 Runtime 更新」+ 更新按钮） |
| Runtime / Step 版本信息格 | **齐** |
| 环境检测清单（勾选/警告图标 + 状态 + 说明） | **齐** |
| 平台显示 | **待确认**：图上是 `win32 · x64`，而捕获机是 Linux。预览模式下走 fixture，所以这是演示数据的值；但要确认真实模式下 `platform` 位读的是真值，不是也落了 fixture |

### 其余四页（首页 / 会话历史 / 能力中心 / 媒体）

页面都渲染出了完整的真实 UI（不是空壳、不是崩溃页），互异。逐模块比对未做——参考集里
没有与它们一一对应的基准图（`__shots/` 的 24 张集中在 workbench / diagnostics /
settings-perms / palette / project-home / agents / approval / streaming / diff-split /
minimap / preview / env-fail 十二个场景）。

## 因此 M4.1 的真实剩余

不是「没做」，是三件具体的事：

1. **IA 差异要一个决策**：设置页分组 taxonomy 与顶栏 chrome 和 omp-studio 不同。
   `comparison-checklist.md` 要求「缺」要么补、要么在 `plan.md` 记决策。现在两者都没做。
2. **按参考场景补抓**：`__shots/` 的十二个场景里，我们能对应的（workbench-streaming、
   wb-approval、wb-agents、wb-diff-split、wb-minimap、wb-preview、palette、
   project-home、env-fail）都还没有对应捕获。harness 已支持 `?route=`，加场景即可。
3. **人眼逐模块过一遍**：B3，无法在本机替代。

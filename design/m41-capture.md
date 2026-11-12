# M4.1 六页面像素签收——捕获仪器与首轮结果（2026-11-12）

## 仪器

`capture.sh`：Xvfb 起显示 → 跑 debug 壳 → `xwd -root` 抓根窗口 → 杀壳。
一条命令产出可复现的截图，不需要人在机器前。

```
./capture.sh /tmp/page.xwd
```

XWD 而非 PNG：本机只有 `xwd`/`xwud`，没有 ImageMagick。XWD 是无压缩位图，
`xwud -display` 可直接看；转 PNG 需要有 `convert` 的机器，未做。

## 首轮结果（workbench 页）

| 项 | 结果 |
|---|---|
| 捕获 | 成功，6.4 MB XWD |
| 壳 stderr | **空** |
| 与 `docs/images/workbench.png` 的逐模块比对 | **未做** |

壳 stderr 为空这一条本身是结论：`STEP_STUDIO_SELF_CHECK=1` 下壳应当打印
`[bridge] session N established` 与逐字流式的 `[self-check]` 行（`main.rs:91`、
`trace_sink`）。两者都没出现，说明 **25 秒等待内壳没走到 setup 完成**——debug
构建在 Xvfb 下 GTK/WebKit 初始化就要 20 秒以上（M2.1 验收时同一现象，当时给了
35 秒）。

所以首轮**没有取得可比对的画面**，不是「比对后一致」。

## 未做的与原因

| 项 | 原因 |
|---|---|
| 逐模块像素比对 | 需要先把等待时间提到 60 s 以上，再对九张图逐一比对；本次预算内未完成 |
| 六页遍历 | 路由存在 `omp.lastRoute`（localStorage，UTF-16LE），可预设后逐页捕获；本次未做 |
| XWD → PNG | 本机无 ImageMagick |

## 下一步（按顺序）

1. `capture.sh` 的等待从 25 s 提到 60 s，重跑 workbench，确认 stderr 出现自检行——
   这是「壳真的跑起来了」的判据，比截图本身更早该建立。
2. 预设 `omp.lastRoute` 后逐页捕获九张。
3. 逐模块比对，结果记入 `comparison-checklist.md` 的对应行。

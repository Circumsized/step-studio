# M4.1 六页面像素签收——捕获仪器与首轮结果（2026-11-12）

## 仪器

`capture.sh`：Xvfb 起显示 → 跑 debug 壳 → `xwd -root` 抓根窗口 → 杀壳。
一条命令产出可复现的截图，不需要人在机器前。

```
./capture.sh /tmp/page.xwd
```

XWD 而非 PNG：本机只有 `xwd`/`xwud`，没有 ImageMagick。XWD 是无压缩位图，
`xwud -display` 可直接看；转 PNG 需要有 `convert` 的机器，未做。

## 第二轮结果（workbench 页）

| 项 | 结果 |
|---|---|
| 捕获 | 成功，**应用自己的窗口** 0x200003，1280×800（= 壳的 `inner_size`），4.1 MB XWD |
| 壳 stderr | 完整自检轨迹：`session 1 established` → `prompt accepted` → 逐字 `he`/`llo`/`!` → `receipt core.prompt -> completed` → `conversation.turn.completed` |
| 与 `docs/images/workbench.png` 的逐模块比对 | **仍未做** |

第一轮失败在两个地方，都已修：① 等待 25 秒太短，debug 壳在 Xvfb 下 GTK/WebKit
初始化就要 20 秒以上，壳根本没走到 setup；② 捕获脚本没传 `STEP_STUDIO_STEP_CMD`，
适配器起不来，daemon 20 秒后把表面关掉，壳报 `Connection reset by peer`。
等待提到 55 秒、补上 fake-step 之后，自检轨迹完整。

第三个问题更隐蔽：`xwd -root` 三次产出**字节完全相同**的文件。无合成器下子窗口
不属于根窗口的 framebuffer，`-root` 只抓到背景。改为按 `xwininfo -root -tree`
找应用窗口 id 再 `-id` 抓，尺寸从 1600×1000（整个 Xvfb 屏幕）变成 1280×800
（壳自己的 `inner_size`）——这才是画面。

## 本文件的结论已过期（2026-11-14）

下面这节「仍不能完成比对的原因」不再成立，两处都错了：

1. **参考图不是 2160×1350**。`ui_reference/ver2/__shots/` 的 24 张**全部是
   1658×846**。尺寸差是 1.3 倍，不是 1.7 倍。
2. **XWD 可以转 PNG**。`scripts/xwd2png.py` 直接解析 XWD 头部（25 个大端 32 位字
   + `ncolors`×12 字节颜色表 + 每行 `bytes_per_line` 像素），用 pillow 出图。
   不需要 ImageMagick。

但更要紧的是：这份文件里「捕获成功」的那张图，转成 PNG 之后是**一张
「Rendererer error」崩溃页**。所以 M4.1 从来连一张可用捕获都没有，而两份记录都
写着「捕获成功、比对未做」。

现在的情况见 [`m41-signoff.md`](m41-signoff.md)：仪器修好了，10 张互异截图在
`design/signoff/`，首轮结构比对已做。

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

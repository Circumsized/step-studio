# AFK 开发工作流

无人值守（away from keyboard）让 agent 跑一个长任务时，照这个走。核心是三条纪律：**先本地验证再推**、**一次只收敛一个根因**、**退出条件机器可判**。

对应问题：B1、B3、B4、C1、C2。

---

## 1. 开工前的三件事

### 1.1 把目标写成机器可判的

不要「修好 CI」。要「`cargo test --workspace` 和 `cargo clippy --workspace --all-targets -- -D warnings` 全绿，且 `gh run view <id>` 五个 job 全 success」。

模糊目标 → 最短路径完成。这是 CodexLoop 实践里记录的头号失败模式：*模型会不会偷懒？会，而且非常频繁……原因不是模型坏，而是目标定义不清 → 最短路径完成*。

### 1.2 列一张 preflight 清单，写进同一个 prompt

无人值守时没有人能拦你，所以清单必须在 prompt 里，不是在脑子里。本次会话如果有这张清单，四个错误不会发生：

```
- [ ] 改动触及 #[cfg(windows)] / #[cfg(unix)] 分支？
      → cargo-xwin clippy -p <crate> --target x86_64-pc-windows-msvc \
          --all-targets -- -D warnings
- [ ] 改动触及 Cargo.lock / 依赖？
      → cargo deny check
- [ ] 改了 .github/workflows/*.yml？
      → node -e "yaml.load(fs.readFileSync(f,'utf8'))" 逐文件验 YAML
- [ ] 新增/改动了脚本？
      → node --test scripts/*.test.mjs
```

第三条是 A9 的直接对策：重复 `if:` 键让 `release.yml` 整个文件失效，GitHub 每次 push 生成一个 0 job 的 run 报 "workflow file issue"，而本地一个字都不报。一行 js-yaml 就能拦住。

### 1.3 设预算和止损

- **轮次上限**：同一个目标最多 N 轮验证循环，N 建议 5。Anthropic Claude Code 团队的做法是*失败三轮即停手，转交人工，避免无限重试*。
- **金额上限**：本次会话到一半时已过千。无人值守任务应该在 prompt 里写死「超过 $X 停下来汇报」。

字节跳动火山引擎把 **Token 刺客、Agent 失控、盲目重调** 并列为「真实的财务和系统灾难」。Huntley 也明确说过：*「这些是精挑细选的成功案例。对于每个 overnight 胜利，都有循环在未收敛的情况下耗尽了迭代次数。失败的尝试仍然花费金钱。」* 他公开的成功案例——三个月跑出一门完整编程语言、YC 黑客松 overnight 6+ 仓库花 297 美元——和那些没收敛烧光的循环是同一批尝试。
- **时间盒**：单轮验证等待超过上一次 2 倍，**先怀疑挂住，不要提高限额**。本次违反了这条，30→45→60 提了三次才发现是测试挂死。

---

## 2. 本地验证优先于推送（本次最贵的一条）

**规则：能在本地验的，一律不推上去验。**

本次四个错误各值一轮 20 分钟的 CI，全部可以在 4 秒内本地发现：

| 错误 | 本地怎么发现 |
| --- | --- |
| `miow::pipe::wait` 不是自由函数 | `cargo-xwin check --target x86_64-pc-windows-msvc` |
| `canonicalize` 的 `\\?\` 前缀 | 同上（编译过之后再想） |
| `Path::exists` 探活 | 同上 + 读代码 |
| 管道名撞车 | `grep -rn 'join("bridge.sock")' crates/` |

Ultralytics 的 CI 指南把「测试在本地通过但在 CI 中失败」的第一条原因列为**平台特定问题**，对策是*在目标操作系统上测试*。没有 Windows 机器时，交叉编译是能拿到的那部分。

```bash
# 提交前的目标平台验证，一次 4 秒
cargo-xwin check  -p step-studio-bridge --target x86_64-pc-windows-msvc
cargo-xwin clippy -p step-studio-bridge --target x86_64-pc-windows-msvc \
  --all-targets -- -D warnings
```

**这是本次复盘里性价比最高的一条。** 我在会话中途才想起来用，之前完全没做。

---

## 3. 修 bug 先全库 grep 这个模式

**规则：动手改之前，先 grep 同一个错误形状在仓库里出现几次。出现第二次，就该在共用函数里修，不在调用点。**

本次 `transport::bind` 的 Windows 语义错了七个测试点。我一个一个修到第七个，才改成让 `bind` 自己转换（幂等）。前六轮是无效改动。

Ponytail 梯子本来就写着这条——*one guard in the shared function is a smaller diff than a guard in every caller*。违反自己挂在会话里的规则，是因为修到第三个时「顺手就改了」，没有停下来 grep。

自检问题：**这个错误形状，除了我眼前这一处，还有哪里有？**

---

## 4. 一次只收敛一个根因

**规则：修复过程中浮现的新故障，写进 deferred 列表，不许当场追。**

本次每修一个就冒出一个新的，我一追到底，于是十轮。CodexLoop 用 `deferred.md` 解决：AI 做长任务时会不断冒出新想法，**立刻做就会一直发散无法收敛**。

deferred 条目格式：

```
- [ ] <一句话故障>
      发现于：<哪个修复轮次>
      阻断当前目标吗：是 / 否
      否 → 当前目标完成后处理
```

判「是否阻断」的标准：不修它，当前退出条件能不能达成？能 → 进 deferred。不能 → 它是当前目标的一部分，继续。

---

## 5. 收尾

无人值守任务的最后一步不是「推上去」，是**如实汇报三件事**：

1. 退出条件达成了没有（贴机器输出，不贴自己的结论）
2. 窄掉了什么——任何一个为了让门禁通过而做的收窄（`#[cfg]` 关门、跳过测试、mock 代替实现），写清楚窄了什么、什么时候补
3. 花了多少（轮次 / 金额 / 墙钟）

第 2 条是 C3 的对策。本次三个套件按 Unix 收口，理由都是真的，但如果不在同一轮里写下来，下一个看到绿板子的人不会知道 Windows 上有三个套件是空的。

---

## 来源

- [详解 Claude Code 之父 Boris 的技巧分享](https://zhuanlan.zhihu.com/p/1991184780939765759)（第 13 条：反馈循环质量）
- [Codex 自主规划开发工作流实践 / CodexLoop](https://developer.aliyun.com/article/1782934)（deferred ideas、模型走捷径）
- [Claude 代码九成由 AI 自审](https://k.sina.cn/article_7879923048_1d5ae156806801fogi.html)（失败三轮即停手）
- [Ultralytics CI/测试工作流](https://handbook.ultralytics.com/zh/workflows/ci-testing)（本地过 CI 挂的平台特定原因）
- [让开发自动化：持续集成反模式](http://www.uml.org.cn/jchgj/201311201.asp)（反馈太少/垃圾反馈）
- [AI Agent 不跑偏的 11 条工程实践](http://www.ksqn.cn/news/22414)（边界感、显式约束）

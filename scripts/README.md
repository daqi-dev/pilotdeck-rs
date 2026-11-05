# scripts — 确定性脚本

> 第三组「脚本与扩展」的另一半：**技能是软的，脚本是硬的。**

## 什么该写成脚本

判据只有一条：**这件事能不能用规则判对错？**

| 能规则判 | 写脚本 |
|---|---|
| 命名对不对、格式合不合规、有没有漏字段、链接通不通 | ✅ 脚本，秒级、可重复、不看心情 |
| 写得好不好、这个方案合不合适 | ❌ 留给人，或留给技能 |

**能用规则判的，绝不留给人判。** 每次你手工重复第二遍的检查，都该问一句：能不能是一条命令？

## 怎么长出来

不要预先设计。**踩到一次坑，就把那次的检查固化成一个脚本**——
它的名字应该是那次坑的名字。

## 约定

- **只放这里，不放全局 `bin/`**——全局对整台电脑可见，容易过载，安全风险也高
- 一个脚本干一件事，名字说清干什么
- 能跑就能重复跑：**同样输入同样结果，跑几遍都一样**
- 报告问题，不擅自修改——要改也另出一个 `fix-*` 脚本，让人决定跑不跑

## delivery-orphan.py — 孤儿交付检查

踩过的坑：**#1333** 交付完成、树上有 commit，却没人开 PR，躺到另一个代理顺路看见；**#1355** 19:22 交付，空等 58 分钟无人接手，靠 20:20 守望扫描才发现。两次都是「靠人记得」，所以固化成确定性检查。

```bash
python3 -I scripts/delivery-orphan.py        # 巡检：有孤儿 → 逐行打印并退出 1，无 → 退出 0，自身出错 → 2
~/ws/pdrs-ops/bin/delivery-orphan            # 同上（ops 薄封装，可换当前目录跑）
~/ws/pdrs-ops/bin/round-start --only-orphans # 只跑 round-start 第 9c 段
```

三类判定（输出一行一条，制表符分隔 `KIND<TAB>树<TAB>对象<TAB>详情`）：

| 类 | 判据 | 输出 |
|---|---|---|
| A 有交付无认领 | `_build/wt-*/_tmp/**/{glm,dsh,codex}.exit`、`glm.status` 的 **mtime** 超过 15 分钟，认领表无记录 | `ORPHAN-UNCLAIMED` |
| A 认领后停滞 | 认领表状态仍是 `claimed` 且超过 45 分钟没更新 | `ORPHAN-STALLED` |
| B 有提交无 PR | 分支非 HEAD/main、`origin/main..HEAD` 有提交、该分支无 open/merged PR、认领表无 `pr-opened`/`merged` | `ORPHAN-NO-PR` |
| C 无人接手 | B 类命中，且本机没有 runner 进程、没有 `remote-wait` 在等它、远端也查不到 | `ORPHAN-DEAD`（最高优先，「没人会再来处理它」） |

阈值与依据（都能用环境变量覆盖，写死在代码里是为了不靠人记得）：

- `PDRS_ORPHAN_UNCLAIMED_MIN=15`：正常收货是「轮询 + auto-receive 数分钟 + 开 PR 数分钟」，15 min 是它的 20 倍余量；今天两次事故的间隔是 58 min，15 min 能在空等半小时内报出来。
- `PDRS_ORPHAN_STALLED_MIN=45`：认领后 45 min 还是 `claimed`，等于一次正常收货加一次重试都没落地。
- `PDRS_ORPHAN_PR_TTL=300`：同一分支的 PR 查询缓存 5 分钟（每分支一次 `gh` 调用；30 棵树冷启动约 20 s，热启动约 3 s）。
- `PDRS_ORPHAN_MAX_AGE_HOURS=72`：A 类只看三天内的交付（更老的流水该显式标 `parked`）；全量翻旧账用 `--max-age-hours 0`。B 类不看年龄，那是仓库状态不是流水。
- `PDRS_ORPHAN_HOSTS=cb8,mbp2,macmini-01`：必查的远端 worker 主机。加机器时改这里，否则新机上正在跑的单会被判成 `ORPHAN-DEAD`。

免误报的闸（都是为了不把「已处理」当成「没人管」）：

1. **交付之后这棵树上又出现提交 = 已收货**（`--no-commit-check` 可关）。#1355 的事故形态是 19:22 交付、20:25 才有人提交——那 58 分钟空档里本脚本照样会报，收货后自动静默。认领后停滞（`ORPHAN-STALLED`）同样吃这道闸：代理收了货但忘了回写 `--status pr-opened` 时，不会把已收的货报成「无人推进」。
2. **「有人在跑」只认 runner 签名**（cargo/rustc/worker/评审/remote-wait/宿主链/auto-receive/负控）。单纯的 `find`、`grep`、`git status` 提到树名不算——否则任何一次巡检自己扫树都会把 DEAD 误判成 NO-PR（11-05 真实现场实测踩到过）。
3. **远端没核过就不判 DEAD**：定不出单号（树名与分支名都不是数字）时把三台主机全记成「未核」，只报 `ORPHAN-NO-PR`。11-05 实测踩过：只查本机会把 cb8 上正在跑的 #1473 误判成「没人会再来处理它」。

**已知豁免（11-05 闸门 P1，不在本脚本修）**：ops 侧守望 `watch-deliveries10.sh` 的扫描 glob 仍是 `_tmp/2026102[6-9]-issue-*`（只到 10-29），11 月起的交付**不会触发 auto-receive**。本脚本是报警端、不依赖守望，故不受影响；守望换 glob 要另起 v11 并先把存量 11 月交付播种进状态文件，否则会一次性全部 auto-receive——单独 ops 跟进，不借本 PR 改运行中的脚本。

### 认领表 `_tmp/20261105-watch/claimed.tsv`

制表符分隔：`轮次目录<TAB>认领时间<TAB>认领者<TAB>状态(claimed|pr-opened|merged|parked)`。写入一律 `flock` + 临时文件 + `rename`（内存 memory 的 lock-use-flock / signal-file-atomic），并发追加安全。

```bash
scripts/delivery-orphan.py --claim _build/wt-1402/_tmp/20261105-issue-1402 --by claude-A          # 派收货代理时写一行
scripts/delivery-orphan.py --claim <同一目录> --by claude-A --status pr-opened                    # 开 PR 后改状态
scripts/delivery-orphan.py --claim <同一目录> --by claude-A --status merged                       # 合入后
scripts/delivery-orphan.py --claim <同一目录> --by claude-A --status parked                       # 长期不动的证据树
scripts/delivery-orphan.py --claims                                                               # 看全表
```

`pr-opened` / `merged` / `parked` 三态都算「已了结」，A 类不再报；`merged`/`pr-opened` 也会让同一单的 B 类静默。

已知边界：远端 worker 默认直接 ssh 问 cb8/mbp2/macmini-01（`--no-check-remote` 关掉），另外兜底本机的 `remote-wait` 等待进程与 `waiter.pid`；**远端有主机没核过就只报 NO-PR、不判 DEAD**——11-05 实测踩过两次：只查本机会把 cb8 上正在跑的 #1473 误判成「没人会再来处理它」，而远端 `pgrep` 的模式若原样写进命令又会自匹配、把每条都判成「有人在跑」（模式末位写成 `[3]` 断开）。A 类只按 mtime 判交付时间，**不读 exit 内容判成败**（exit 文件内容不可信，且失败交付同样要有人处理）。


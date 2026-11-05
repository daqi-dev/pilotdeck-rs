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
python3 -I scripts/delivery-orphan.py        # 巡检：有孤儿 → 逐行打印并退出 1，无 → 退出 0，自身出错/没核成 → 2
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
- `PDRS_ORPHAN_LEDGER_TTL=5`：认领表快照 5 秒后重读。巡检要跑几分钟，而 `--claim` 恰恰是有人在响应告警时才写的；只读一次会把刚认领的单继续报成 `ORPHAN-UNCLAIMED`/`ORPHAN-STALLED`（假告警）。
- `PDRS_ORPHAN_MAX_AGE_HOURS=72`：A 类只看三天内的交付（更老的流水该显式标 `parked`）；全量翻旧账用 `--max-age-hours 0`。B 类不看年龄，那是仓库状态不是流水。
- `PDRS_ORPHAN_HOSTS=cb8,mbp2,macmini-01`：必查的远端 worker 主机。加机器时改这里，否则新机上正在跑的单会被判成 `ORPHAN-DEAD`；设成空串/全逗号按默认表处理（`VAR=` 是 shell 里「恢复默认」的写法，静默清零会让每棵树直接判 DEAD）。

免误报的闸（都是为了不把「已处理」当成「没人管」）：

1. **交付之后这棵树上又出现提交 = 已收货**（`--no-commit-check` 可关）。#1355 的事故形态是 19:22 交付、20:25 才有人提交——那 58 分钟空档里本脚本照样会报，收货后自动静默。认领后停滞（`ORPHAN-STALLED`）同样吃这道闸：代理收了货但忘了回写 `--status pr-opened` 时，不会把已收的货报成「无人推进」。
2. **「有人在跑」只认 runner 签名**（cargo/rustc/worker/评审/remote-wait/宿主链/auto-receive/负控）。单纯的 `find`、`grep`、`git status` 提到树名不算——否则任何一次巡检自己扫树都会把 DEAD 误判成 NO-PR（11-05 真实现场实测踩到过）。
3. **没核成 ≠ 没有**：远端把 `pgrep` 自己的退出码回传（`pgrep -f … >/dev/null 2>&1; echo rc=$?`，不再走 `| wc -l`——管线退出码是 `wc` 的 0，远端没装 pgrep(127) 或报权限错(2) 都会打印 `0` 被当成「核过且没人」，三台都这样就是假 `ORPHAN-DEAD`）。`rc=1` 才算「核过、没人」，其余与 ssh 本身失败、输出不成形一律记「未核」，只报 `ORPHAN-NO-PR`。本机同理：`pgrep` 只有 `rc=1` 算「没人」，超时(124)/不可用(127) 记「本机未核」。定不出单号（树名与分支名都不是数字）时把三台主机全记成「未核」——11-05 实测踩过：只查本机会把 cb8 上正在跑的 #1473 误判成「没人会再来处理它」。

```bash
python3 -I scripts/test-delivery-orphan.py   # 正控/负控逐条钉住三条不变量（条数以脚本输出为准）
```

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
没有单号的树（`wt-tools-main`、`wt-frozen-batch` 这类工具树）按**树名归属**对 B 类静默，`parked` 同样有效——否则它们永远报 `ORPHAN-NO-PR`、退出码恒为 1，真孤儿被噪音稀释。

退出码契约：`0` 无孤儿 · `1` 有孤儿 · `2` 自身出错**或没核成**（有树因 gh/git 探测失败被跳过、而其余又没命中时，不能报「无孤儿」）。

已知边界：远端 worker 默认直接 ssh 问 cb8/mbp2/macmini-01（`--no-check-remote` 关掉），另外兜底本机的 `remote-wait` 等待进程与 `waiter.pid`；**远端有主机没核过就只报 NO-PR、不判 DEAD**——11-05 实测踩过两次：只查本机会把 cb8 上正在跑的 #1473 误判成「没人会再来处理它」，而远端 `pgrep` 的模式若原样写进命令又会自匹配、把每条都判成「有人在跑」（模式末位写成 `[3]` 断开）。A 类只按 mtime 判交付时间，**不读 exit 内容判成败**（exit 文件内容不可信，且失败交付同样要有人处理）。**`waiter.pid` 只 `kill(pid,0)`**：PID 被无关进程复用时会把死等待判成「有人在跑」，漏报 DEAD（失败方向朝静默，不是假告警）；要堵需在 pid 文件里同时记进程起始时间，见闸门 P2，未在本轮修。

## check-taskbook-segments.py — 大包任务书必须切段

踩过的坑：**2026-11-05 夜 #1316** 跑满 7200 s 被 kill，16 个文件写完但 RESULT / 负控 / final.patch 全缺，只能靠 WIP 补丁续跑；同一晚 #1334 #1336 #1370 #1328 #1339 同类超时。
「每 30 分钟 WIP 提交」只防丢进度，**不防「一整段跑不完」**——所以要在大包派单前就按文件切段。

判据来自最近 10 个超时单与 10 个一次跑完的单：**手写改动文件 ≥ 8 或 手写 diff ≥ 1500 行**（召回 9/10，误报 3/10，且误报的三个分别是 95 min 逼近上限、478k 行生成金样、接续轮）。
派单前拿不到真实改动量，所以任务书写 `<!-- pdrs-scope: files=N lines=M est_min=K -->` 声明；没写才退回人日估算（约 / 大约 / 估计 / 预计 / 估算 / 工作量 / 共计 / 合计 / 总计 … 可带冒号逗号 … N 人日）与点名 `*.rs` 路径数。

```bash
python3 -I scripts/check-taskbook-segments.py <任务书>     # 0 通过（含小包）/ 1 大包缺分段表 / 2 读不了、为空、读取中被改写或 scope 写坏 / 3 用法错误
python3 -I scripts/check-taskbook-segments.py --print-skeleton
python3 -I scripts/check-taskbook-segments.py --recent 24 [--root .]   # 兜底递归扫 _build/wt-*/_tmp/**/task.md，没扫到也是 0
python3 -I scripts/test-check-taskbook-segments.py        # 117 条正控 / 负控（负控钉死具体报错文案）
```

命中大包的任务书必须含 `## 分段交付`（段名 / 涉及文件 / 交付物 / 预估时长 ≤90 min / 段间依赖，≥2 段；列序不限，按表头定位：先精确 `段名` 再退回 `段`，`段间依赖` 先认领依赖列，同一列不会被两列认领）与 `## 续跑`（带目录的实例进度文件路径 + 第 N 段失败从哪继续）。
占位符按「像占位符」判：尖括号/花括号里含中文、是纯点号（`<...>` / `{…}`）或是 `N` / `日期` / `待填` 这类词，外加裸 `...` / `…` / `？？？` / TBD / TODO；`Vec<u8>`、`Result<(), E>`、`<https://…>` 这类仓库常态不误伤。估时区间 / 上限（`80–120 min`、`80/90 min`、`上限 90 min`）取最大值，并列或连写（`60 min + 40 min`、`1h40m`）求和；区间与加法同现时先按加法切段、每段取 max（`40–50 min + 20 min` = 70，不是 110；裸「增加 / 加班」不算加法）。单位只认 min/分钟/m/h/小时（`以内` / `之内` / `以下` / `左右` 这类限定词不算单位的一部分）；表头写 `(h)` / `(hrs)` / `工时` 时裸数字按小时算，写 `(人日)` / `工作日` 判「读不出」。
代码围栏（``` / ~~~）里的**分段表与占位符**不算数：贴上一单的示例分段表不能顶替本包的，围栏里残留的 `<段名>` 也不会把合规任务书打回。**scope 声明只认围栏外正文**；人日与 `.rs` 回退看原文（围栏里的证据也算，宁可判大）——围栏里贴一段示例 `<!-- pdrs-scope: files=2 -->` 不再把 9 个 `.rs` 的包短路成小包，围栏里写坏的示例 scope 也不再假 ERROR。**围栏开了没收口** rc 2（半截围栏后面是正文还是示例判不准）。
`--recent` 递归扫 `_build/wt-*/_tmp/**/task.md`（修复 / 续跑 / 重生成轮次在 `fix-rN/`、`resume*/`、`regen*/` 下一层，只认一层会整类漏掉），但跳过 `_tmp` 之后路径段里叫 `remote-run` / `remote-run.*` / `.prev-*` 的派发副本与旧轮次备份，同一份任务书不重复判（只看 `_tmp` 之后：`--root` 前缀里叫 `remote-run` 不能把所有任务书跳光）。
`--recent` 与显式任务书可以同用：单个扫描到的文件在扫描期间被回收仍静默跳过（那不是任务书的问题），但**遍历本身**抛 OSError 一律 rc 2；`--root` 只与 `--recent` 同用（单独给 `--root` 是用法错误，免得以为按 root 解析）；**显式点名的文件不见了一律 rc 2**；读后复核 size/mtime，只挡 stat→read→stat 这个毫秒窗口内的写入——**写方停顿超过 10 s 再续写的半截任务书挡不住**（要堵得靠写方原子落盘），别当成已解决。空扫描 `--json` 打 `[]`、`--quiet` 只留 stderr 提示；**递归扫描中断（glob 抛 OSError）→ rc 2**，不与「扫过但没扫到」（rc 0）同观感（上句的「单个文件被回收」不在此列）；mtime 比 `now - 10 s` 新的一律跳过（含未来时间戳，宁可漏一轮也不读半截）；非空 `--json` 只输出 JSON、`--explain` 只打每个文件的判定依据，两者都不打文本 SUMMARY。非普通文件（FIFO / 设备文件）一律 rc 2，不试着打开（会永久阻塞）；读到一半文件被删也 rc 2。
`~/ws/pdrs-ops` 的 `dispatch-issue` / `dispatch-fix` / `fix-assemble` 已强制：不合规不启动 worker；`dispatch-resume` 只告警；`round-start` 9d 段每轮兜底。模板见 `~/ws/pdrs-ops/templates/taskbook-segments.md`。

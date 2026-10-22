# stats Date.parse 与账目保留金样

真相源是 PilotDeck `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的
`src/router/stats/TokenStatsCollector.ts:297-302`。`generate.mts` 先调用公共溯源守卫核对
HEAD、工作区及源码 SHA，再动态导入真实 TS collector；期望毫秒由真实 Node
v22.22.0 / V8 12.4.254.21-node.33 的 `Date.parse` 产生，保留判断调用真实
`TokenStatsCollector.prototype.isRetained`，压缩与重开结果来自真实 collector。
没有手写日期期望值，没有调用 Rust 生成期望，没有改写 TS 或依赖。

```sh
PDRS_BASELINE=/abs/PilotDeck node --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/stats-dateparse/generate.mts /tmp/owned-empty-output
```

生成两次，逐字节比较 `oracle.json` 与 `local-profiles.json`；同时审阅新数据后更新这两个正本。
Node、V8、ICU、ICU tzdata、基线 commit 与源码哈希均在产物中。输出目录必须为空；
原始 journal 与压缩后 journal 另保留在每个生成目录的子目录中。

`oracle.json` 包含同一组 15,887 个输入和三个独立进程的真实观测：`TZ=UTC`、
`America/New_York`、`Asia/Shanghai`。每项保存 `Date.parse` 结果（NaN 编码为 null）、
固定时钟保留结果、以有效毫秒为界的 -1/0/+1ms 保留结果。输入覆盖规范 ISO、
扩展年份、TimeClip 两端、日历翻转、24:00、不同长度小数秒、数字偏移、RFC 2822、
月名前缀、两位年、括号嵌套/未闭合、时区缩写、垃圾文本、空白、NUL、单独代理码元、
多字节偏移、Node 解码的畸形 UTF-8、确定性随机与语法变异。三个 journal 均直接写入
7 组畸形 UTF-8 原始字节，真实 TS 读取替换后压缩，Rust 用同一组原始字节逐字节回放。
三个时区还分别对真实
journal 的首次重建、压缩字节、聚合、重开快照对拍。

本地时间直接复用 `pilotdeck-transport::system_clock` 的 V8 移植和 TZif 解析器：
跳时缺口采用跳时前偏移（保留原分钟/毫秒），重复时刻采用较早 UTC 时刻；不推算当前偏移。
读取进程 `TZ`，未设置则读取 `/etc/localtime`，与 TS 的实际部署进程保持相同 TZ。
显式时区与 ISO date-only 不依赖本地数据。

**保守边界**：Node ICU 为 tzdata 2026a，生成机器系统为 2026d，不能把任意系统时区当作
同版 ICU。`local-profiles.json` 绑定上述三个时区的实际 TZif 字节 SHA；语料包含这些文件中
每次历史转换的两侧本地边界（含 ±1ms）、纽约/上海春秋跳时、2038 后完整 400 年周期样本
和远古/扩展年份。stats 只接纳这三个已对拍的 **TZ 标识 + 字节 SHA** 配对；未设置 TZ 时，只有
`/etc/localtime` 的规范链接指向同名系统 profile 且字节匹配才可用。未验证别名（包括
`localtime`、`:Asia/Shanghai`）、未知/变化的 TZif、POSIX TZ、自定义路径、不可读数据或共享解析器无法
确定的本地时间均保留。升级系统时区后必须重新运行真实 TS 对拍再登记 profile。
宿主字节未登记时，测试仍核对显式时区/ISO date-only，并检查本地时间保留；输出
`PARTIAL` 和未对拍本地项数，跳过该时区真实 journal 的精确对拍，不能当作本地时间已对齐。
本单实际宿主三个 profile 均匹配，47,661 个值与三个 journal 全部精确对拍。

实现复用的上游源码（只读核实，未下载二进制）：
- Node v22.22.0 `deps/v8/src/date/dateparser-inl.h:228-338`：ISO 路径。
- 同文件 `:14-172`：ISO 未处理 token 与组件进入 legacy 路径；`:175-225` 是 tokenizer。
- `deps/v8/src/date/dateparser.h`：reader、token 与各 composer；`dateparser.cc`：composer 写出、
  关键字表及毫秒截断；`src/objects/intl-objects.cc:2508-2519`：ICU FORMER/FORMER。

回归命令：`cargo test -p pilotdeck-gateway --lib stats_dateparse -- --nocapture`（在 `src/` 下）。
子进程在启动前固定 TZ，不修改并行测试进程的环境。另有 10,000 轮随机 UTF-16/字节串和
每个偏移插入多字节字符的无 panic 回归。变异负控与各项原始输出见本单 RESULT。

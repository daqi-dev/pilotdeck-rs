# 决策卡：#1324 A8.3-pr1b cron 调度计算的时区数据源（2026-11-02，自决 · 待复核）

- **卡在哪**：#1324 要在 `runtime/cron/` 移植 TS `computeNextRunAt`（CronSchedule.ts 用
  `Intl.DateTimeFormat` 按时区逐分钟扫候选）。TS 侧是 Node 内置 ICU 78.2 tzdata，无 npm 依赖；
  Rust 侧仓里只有 zone **名单**（`src/pilotdeck-gateway/src/config/timezones.json`，597 个 ICU
  记录 zone），没有 tz **规则** crate，任务书又禁止下载预编译二进制。需要定：有效性判什么、
  规则（UTC→local 偏移）从哪来。
- **能否撤回**：能（只影响本单新增组件与 transport 一个新类型，接线在 #1325，换数据源不动外层）。
- **动不动尺子**：不动。

## 候选

| 方案 | 做法 | 问题 |
|---|---|---|
| A. 新增 chrono-tz 等 tzdata 内嵌 crate | 依赖里带一份编译进二进制的 tz 规则 | 违反「不下载预编译二进制」；其 tzdata 版本与 ICU 78.2 无对齐保证，DST 时刻漂移不可检 |
| B. 从 Node ICU 78.2 探测生成规则表并入库 | 生成器扫出每 zone 的 transition 表 | 597 zone × 1970–2100 规则约 MB 级数据 + 复杂生成器；本单组件未接线，过度 |
| C. 系统 zoneinfo（transport 既有 checked TZif 解析器，tz-rs 0.7.3） | 规则来自 `/usr/share/zoneinfo`，解析一次复用 | 宿主数据版本不等于 ICU 78.2；Ubuntu 24.04 拆掉的 backward 别名系统里没有 |

## 决定（C + 记录名单，与仓里已上线件同构）

1. **有效性**（`isValidCronTimezone`，TS 是 `Intl.DateTimeFormat` 构造 try/catch）：复用
   `config/sections.rs::valid_timezone`——记录的 ICU zone 名单（597 个）+ UTC offset 正则
   （`^[+-]HH(:?MM)?$`）。这不是巧合：该名单本就是从同一 Node 22 / ICU 记录的，注释即
   「Node 22 accepts case-insensitive ICU zone IDs plus UTC offset identifiers」。
2. **规则**（UTC→local 偏移）：`pilotdeck-transport` 的 `system_clock` 已有的 checked TZif
   通路（`tz-rs 0.7.3`，先精确后大小写不敏感匹配系统 zone 树，canonicalize 防逃逸）——
   已上线的 `get_current_time` 工具（builtin/time.rs）就是这条通路。本单给它加一个
   **只解析一次**的 `ZonedPartsResolver`（分钟级扫描最多 527,040 个候选，不能每次重读
   TZif 文件），既有 `zoned_datetime_parts` 重构为它的一次性包装、行为不变。
3. **已知分歧登记**（新开 `specs/gateway-parity/diffs/` 条目）：ICU 接受但宿主 zone 树
   没有的旧别名 zone，Rust 规则解析 fail-closed → `computeNextCronRunAt` 返回 `None`
   （TS 会算出日期）。与 `system_clock.rs` 头注的既有立场一致：「system TZ data equivalence
   to pinned ICU remains a separately exercised profile」。

## 证据（2026-11-02，Node v22.22.0 / ICU 78.2 实测，探针原件 `_tmp/20261026-issue-1324/`）

- **数值等价**：597 个记录 zone × 8 个探测时刻（2026/2027/2030，含 3 月/4 月/10 月/11 月 DST
  边缘），系统 zoneinfo（cb8，Ubuntu 24.04 tzdata **2025b**）存在的 496 个 zone 与 Node ICU
  78.2 的 month/day/hour/minute/weekday **零差异**（4776 行逐格比对，`tz-probe/`）。
- **接受范围对拍**：38 个边角输入（`utc`、`ASIA/SHANGHAI`、`US/Pacific`、`EST`、`PST8PDT`、
  `Etc/GMT+5`、`+05:30`、`+05`、`-0800`、`+050`(F)、`-08:0`(F)、`UTC+8`(F)、`UTC0`(F)、
  `Factory`(F)、`localtime`(F)、`+24:00`(F)、`Etc/GMT+13`(F)、`GMT0`、`Zulu`、`Iceland`、
  `""`(F)、尾随空格(F)……）与 `valid_timezone` 名单+正则逐条一致。
- **缺口**：101 个 ICU 旧别名（`US/*`、`Canada/*`、`Asia/Calcutta`、`Europe/Kiev`、`GMT0`、
  `Zulu`、`W-SU` 等）不在 cb8 系统树（Ubuntu 24.04 的 tzdata/tzdata-legacy 拆分），即上述
  第 3 点登记的分歧。macOS 系统 zoneinfo 含 backward 链接，该缺口更小；金样计算格只用
  两边都存在的 zone，保持宿主无关。

## 用户可感知差异

未接线组件（无用户面）；接线后（#1325）：legacy 别名 zone 的任务在缺 backward 链接的
Linux 宿主上算不出 nextRunAt（表现为任务不调度），其余 zone 与 TS 一致。

## 复核请求

- 是否接受「有效性=ICU 名单、规则=系统 TZif」的分层（而不是把 B 方案的生成规则表做进仓库）；
- 是否接受 legacy 别名 fail-closed 的登记方向。

先说明：我只能看到你贴出的 `summary.json`、`batch.json` 和 10-14 的 `calibration-summary.json` 三份附件。你列出的 `ts-ruler-*.json`、`final-ruler-*.json`、`attempts/`、`c-glm.md`/`c-ds.md`、`acceptance.md`/`README`/`CLAUDE.md` 正文、`git diff` 输出、`approval/` 下的历史 ruler 文件等，我都没有内容可读。因此凡是需要逐字比对 ruler 内部字段（尤其 ③ 的 workspace_packages / 依赖摘要）的，一律标注「需核对」，不下 P0/P1 结论。

---

## ① 定标是否满足冻结程序

可据附件确证的部分：

- **同日**：`summary.utc_day = 2026-10-18`，且 `batch.utc_day = 2026-10-18`，20 个 ref/ts/proxy 每次 `identity.capture_utc_day` 均为 `2026-10-18`。→ 满足「同 UTC 日」。
- **20/5/1**：`runs` 含 ref1–ref20（20）、ts1–ts5（5）、proxy（1），全部 `status: PASS`；`ts_pairs_pass 10 / ts_pairs_expected 10`；`proxy: PASS`；`all_26_clocks_pass: true`；`clock_regression_check` 26 项全 PASS。→ 数量与机器门满足（以 summary 自报为准）。
- **HEAD 与装置一致**：`summary.head = batch.head = ce77149…650`，与题述 `ce77149` 一致；`pinned.source.recorder_proxy` 与 `build.sha256` 均为 `ddf42642…7a6c`，且每次 `identity.source_sha256.recorder_proxy` 都是同一个 `ddf42642…7a6c`。→ HEAD 与 proxy 二进制一致，装置在同一批次内一致。
- **仓外 checkout**：`batch.root = /Users/da/ws/pdrs-b2cal-20261018`，所有目标/二进制/cargo-home 都在此根下（`cargo-target`、`cargo-home`），`resource_root` 才指回 `/Users/da/ws/pilotdeck-rs`。→ 与题述「仓外 detached checkout」自洽。
- **机器门**：`verdict: PENDING_REVIEW`（未自封冻结，符合「待评审」语义）；`frozen: false`；`error: null`。

需要核对而不能据附件下结论的：

- 「同日」是否与 10-14 批准批次属于**不同** UTC 日从而必须重定标、以及冻结规则对「同日」的具体判定口径 → 需核对 `CLAUDE.md`「冻结尺子的变更」原文与 `acceptance.md`。
- 「装置一致」除 proxy sha 外，是否还要求 pinned 脚本集（`pinned.apparatus` 那串 hash）与本批一致、以及是否要求 `git diff` 中的 94.1/94.2/CAP_BYTES 变更都已在 `ce77149` 内 → 需核对 diff 与 pinned 清单。
- `summary` 的 `verdict` 仍是 `PENDING_REVIEW`、`independent_review: null`：程序是否要求先有独立复核再重冻结 → 需核对 `CLAUDE.md`/`acceptance.md`（附件里 10-14 的同字段也是 `null`，说明历史上可能不强制，但不能据此推定）。

**① 小结**：在附件可见范围内，同日、20/5/1、HEAD 与 proxy 二进制一致、仓外 checkout、机器门全过这几点都能对上；程序性细节（pinned 脚本集、独立复核是否强制、CAP_BYTES 对齐是否已入 diff）需核对，暂不判 P0/P1。

---

## ② ref16 重采是否合规、有无偷换参考池

附件可证：

- `attempts` 里 ref16 有两条：先 `REFUSED`（`exit_code: 2`，archive 指向 `attempts/ref16-8e1eb954…`），后 `PASS`（`exit_code: 0`，archive 指向正常 `ref16`）。即**只重采了 ref16 这一遍**，其余 ref/ts/proxy 均单次 PASS，没有整批重跑。
- 重采后 ref16 的 `identity.source_sha256`（node / baseline / node_modules / recorder_proxy）与其它 25 次**完全相同**（`node e6019c…`、`baseline c248d6…`、`node_modules a619ea…`、`recorder_proxy ddf426…`），`fixture_path` 同样是 `slot/direct`，`capture_utc_day` 仍是 `2026-10-18`。
- `batch.pinned.source` 与各次 `identity.source_sha256` 一致，未被替换。

据此，**从附件看没有偷换参考池的迹象**：参考池的 node/baseline/node_modules 摘要与其余样本一致，proxy 二进制也一致。唯一的「变化」是 ref16 这一遍本身重采。

但以下需核对、不能据附件断合规：

- REFUSED 的原因是否确为「wall clock regression relative to monotonic clock」（附件只给了 exit_code 2，未给日志正文），以及程序是否允许「仅重采该遍」而非重跑整批 → 需核对 `attempts/ref16-8e1eb954….log` 与 `CLAUDE.md`/`acceptance.md` 的失败重采条款。
- 重采是否在同一 UTC 日内、且期间未改装置 → 时间上 ref16 PASS 的 `wall_start_ms 1792303632189` 夹在 ref15（…308138）与 ref17（…825028）之间，日内顺序自洽；装置 sha 未变。可作旁证，但「重采合规」的规则依据仍需核对程序原文。

**② 小结**：附件层面 ref16 重采是单遍隔离重采、参考池摘要未变、无偷换证据；合规性（允许单遍重采的规则依据、REFUSED 日志确认）需核对，暂不判 P0/P1。

---

## ③ 与 10-14 批次相比，ruler 结论是否一致（重点 workspace_packages 与依赖摘要）

**这是本评审最关键的缺口，必须「需核对」。**

- 我手里只有两批的 `summary.json`/`calibration-summary.json`（顶层字段），**没有** `ts-ruler-*.json`、`final-ruler-*.json` 的内容，无法比对 ruler 的结论体（含 `workspace_packages`、依赖摘要、ts 对结论等）。
- 从 `batch.json` 能看到本批 `node_modules.coverage.workspace_packages = ["apps/desktop","packages/sdk","ui"]`，`node_modules.sha256 = a619ea…`；`pinned.source.node_modules` 也是 `a619ea…`，两者一致。但 10-14 批次的 `node_modules` 摘要与 `workspace_packages` 我**没有**（10-14 的 `calibration-summary.json` 不含这些字段），无法确认是否「不变」。
- 依赖摘要是否不变：需核对本批 `node_modules.sha256` 与 10-14 批准材料（`specs/ui-server-parity/approval/` 下的 ruler/batch）中的对应摘要是否相等。

所以 ③ **无法判定一致或不一致**，一律「需核对」，不据此下 P0/P1。

---

## ④ 是否同意据此重新冻结

**不同意在此刻直接重冻结**，理由不是发现 P0/P1 缺陷，而是**冻结所需的比对证据我拿不到**：

- 重新冻结要求「登记新的审批依据」，即需把 10-14 与 10-18 两批 ruler 并排证明结论一致（尤其 `workspace_packages`、依赖摘要、ts/final 结论）。这些文件我没有内容，无法确认一致，也就无法背书「据此冻结」。
- `summary.verdict` 仍为 `PENDING_REVIEW`、`independent_review: null`：是否必须先补独立复核再冻结，需核对程序。

**结论**：机器门与装置一致性层面没有明显问题（附件范围内），但 ③ 的 ruler 一致性比对缺失、② 的重采规则依据未核，因此在现有附件下**不能同意**重新冻结；补齐上述「需核对」项且均满足后，方可去掉草稿标记并登记新审批依据。

---

### 分级

- P0 = 0、P1 = 0（附件范围内未见硬缺陷；未定项的均因证据缺失标「需核对」，不升为 P0/P1）
- P2：需核对项偏多且集中在冻结必需的 ruler 比对与重采规则上，属于「未闭环」，但非确定性缺陷 → P2 记 3（ruler 一致性比对缺失、ref16 重采合规依据缺失、独立复核/程序条文未核）
- P3：建议在审批材料中显式记录 10-14 与 10-18 两批 node_modules/proxy 摘要对照表与 ref16 重采事件说明，便于后续审计 → P3 记 2

VERDICT: P0=0 P1=0 P2=3 P3=2
冻结结论：不同意
---
id: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R16
original: A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001
---

## 类别

更正件（R 链，承接 `A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R15`；frontmatter
`original` 按账本机器规则指基础 ID，与 R9-R15 同法，承接关系由文件名与正文表达）。
更正对象是 R15「机器判据」节对 #2692 负控名册的登记数字：fix-r3（提交 `f65f6692b`）
给 `native-blankkey-submit/negative_controls.py` 新增第 12 行变异
`session-id-uses-router-key`（`:220-224`）后未同步账本，R15:119 的「十一行」与
R15:128-129 的静态核输出「`#2692 11 mutation anchors OK; 8 literal anchors OK`」
按登记命令已不可复现（验收证据缺口，sibling P1）。本件按实测更正为名册 12 行、
字面锚 9 个；R15 正文原字节不动，四族差异（①标签碰撞/结构键控、②accept-catch、
③真服务终态、④折叠带）的语义登记不变。

## 基线依据

本 worker 树（远端 cb8，HEAD `f65f6692b182`，与本机主仓同提交）实跑
`python3 -I src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/negative_controls.py --check`，
原始输出（exit 0）：

```
#2692 12 mutation anchors OK; 9 literal anchors OK
```

R15 登记的两处计数各差一，差量均为 fix-r3 的新增行及其字面锚。TS 侧形状零变化
（R15 的基线探针与四族差异判定不受本更正影响）。

## Rust 行为

零变化。本件不登记任何新的 Rust 行为差异，只更正负控装置的机器判据登记：fix-r3
的新增行把 SessionContext 交接折叠回 router 键（`session_id: router_key.clone()`，
对应生产代码 `submit.rs:1043` 的 `session_id: run.session_key.clone()`——fix-r3 条1
的双域键分离），使非字符串 sessionKey 回合死在生产工厂的 `create_storage`
（`std::fs` 拒绝 raw 值标签的 NUL 字节）而到不了模型解析、null.jsonl 不落盘。该行
绑定测试 `native_process::methods::config::tests::
real_services_null_session_key_turn_dies_at_model_resolution`（`config.rs:2609`，
a1 真服务链格；R15:106 登记的 `:2592` 为 fix-r2 树行号），断言锚
`"#2692 the agent_status message"`（`config.rs:2643`）。`EXPECTED_NAMES` 名册
同步为 12 行（第 12 项 `session-id-uses-router-key`），`registered()` 的名册等值
核验自洽；`LITERAL_ANCHORS` 9 项，第 9 项为新增的
`(GATES, '#2692 the agent_status message')`（9 = R15 登记的 8 + 该行）。

## 范围

只覆盖 R15 机器判据节的负控登记数字（名册 11→12 行、字面锚 8→9、静态核输出
原文），附带 fix-r4 同轮的夹具名册同步（`native-blankkey-submit/README.md`
「变异名册（12 行）」与绑定测试列表补 a1 用例）。不覆盖、不改动：R15 四族差异的
语义与批准状态；`turn-submit/negative.py` 等连带重锚行（R15 已登记，fix-r3 后
零变化）；差异①的标签碰撞/结构键控未修项（仍在案待人复核裁决）。不放宽判分、
不新增判分豁免。

## 机器判据

- 静态核（本树实测，`f65f6692b`）：
  `python3 -I src/pilotdeck-gateway/tests/fixtures/native-blankkey-submit/negative_controls.py --check`
  → `#2692 12 mutation anchors OK; 9 literal anchors OK`（exit 0，原文见「基线依据」）；
- 名册等值：`EXPECTED_NAMES` 12 行，第 12 行 `session-id-uses-router-key`，
  绑定 `native_process::methods::config::tests::
  real_services_null_session_key_turn_dies_at_model_resolution`；字面锚
  `LITERAL_ANCHORS` 9 项，第 9 项 `(config.rs, '#2692 the agent_status message')`；
- 夹具 `native-blankkey-submit/README.md` 名册 12 行、绑定测试列表含上述 a1 用例
  全名；
- 账本闸门：`check_frozen.py` 的 `check_diffs` 规则要求本件 frontmatter `original`
  写基础 ID（已写 `-001`，非 `-R15`）；全量杀灭轮（需 cargo 与净树）由协调者
  提交后在宿主跑，杀灭判据以 `EXPECTED_NAMES` 12 行为分母。

## 批准记录

未批准，非已接受差异，待人复核（可否决）。2026-11-12，#2692 fix-r4 worker AI
代登记。本件是登记数字更正（R15 账本与 fix-r3 代码名册脱节、验收证据按登记命令
不可复现），不改变任何差异族的行为语义、判分规则或豁免；R15 原字节不动，账本
只追加。人复核否决则循账本规则出 `-R17` 更正。

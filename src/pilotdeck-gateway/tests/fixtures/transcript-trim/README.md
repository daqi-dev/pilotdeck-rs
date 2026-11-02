# transcript 去空白合并夹具（#1244）

`storage/transcript_adapter.rs` 原有一份独立同形的 25 码点
`js_trim_units`（`0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a |
0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff`），没有任何测试，
与 `runtime::memory::js_is_space_unit`（#1226 已用 Node v22.22.0 全码元
实测穷举钉住）各自修改时会悄悄分叉。#1244 删掉这份私有拷贝，存储侧
（`transcript_adapter.rs` 的 `non_empty_string` / `project_session_info`
与 `session_domain.rs` 的 `trimmed`）统一调用
`crate::runtime::memory::js_trim_units`。

依赖方向不新增环：`storage` 已经 import `crate::metadata`，而 `metadata`
本身调用 `runtime::memory::js_trim_units`（manager 列表路径）；同 crate
内模块互引合法，`runtime::memory` 不引用 `storage`。

## 测的是什么

- `runtime::memory::gateway::tests::js_is_space_unit_pins_the_ts_trim_whitespace_set`
  （#1226）：共享谓词对全 UTF-16 码元的穷举钉扎，期望集是 Node v22.22.0
  实测的 25 码点字面量表。
- `storage::transcript_adapter::trim_tests::non_empty_string_pins_the_js_trim_whitespace_set`
  （#1244 新增）：对存储侧最直接的 trim 消费点 `non_empty_string` 做
  0..=u16::MAX 穷举钉扎（同一份 25 码点期望集，Node v22.22.0 重测，
  #1244 交付证据 `node-probe/whitespace-probe.log`），另加 U+180E /
  U+200B 非空白反例。断言消息带 `#1244` 专属标记，供负控绑定点名。

## 负控

`negative_controls.py`：共享 `negative_suite.run_cases` 执行器，逐项
「正控先绿 → 应用变异 → 绑定测试判杀（点名测试 + 断言消息）→ 字节还原 →
还原后再跑」，全表跑完才汇总，任一非 KILLED 非零退出；`--audit` 在跑
任何东西之前拒绝过期锚点，`--compile-only` 对每个变异先做
`cargo test --lib --no-run` 编译检查（编译失败判 INVALID，不当成判杀）。

4 行变异：

| 变异 | 位置 | 改动 | 绑定测试 |
| --- | --- | --- | --- |
| `shared-unit-00a0-dropped` | `runtime/memory/mod.rs` | 删 `\| 0x00A0` | #1226 穷举单测 |
| `storage-trim-dropped` | `storage/transcript_adapter.rs` | `non_empty_string` 去掉 trim 过滤 | #1244 穷举单测 |
| `storage-trim-rust-whitespace` | `storage/transcript_adapter.rs` | 改用 Rust `char::is_whitespace`（历史分叉形态；U+0085 / U+FEFF 分歧） | #1244 穷举单测 |
| `shared-trim-end-off-by-one` | `runtime/memory/mod.rs` | `js_trim_units` 的 `..=end` 改 `..end` | #1244 穷举单测 |

新文件提交前不在 HEAD，`mutating_sources` 拒跑：提交后由协调者在宿主
执行（`CARGO_TARGET_DIR` 由调用方设置，建议同时设
`LANG=en_US.UTF-8` 并清空 `LC_ALL` / `LC_MESSAGES`——本机默认 zh-CN 时
native archive / plugins catalog 等 19 个与本题无关的测试会因 locale 失败）。

# config-error-names 负控（#1833）

本目录是 #1833（config/binding 错误类名 + 共享 `[object <tag>]` 源）修复轮的负控：
13 个变异体，每个破坏一处错误类决策，要求目标测试以带 `#1833` 前缀的断言消息失败。

覆盖面（与 `negative_controls.py` 的 `mutations` 表一一对应）：

- `binding-join-typeerror-name` / `read-error-node-class` / `sops-parse-error-class` /
  `sops-conversion-error-class`：四个错误构造点丢掉 `name` 覆盖，落回默认
  `PilotConfigError`（A1/A2）。
- `helper-collapses-to-notprimitive` / `helper-collapses-to-stackoverflow`：
  `require_string_coercible` 把错误种类折叠成单一 kind（B1 的两类分别钉住）。
- `schemaversion-coercion-check-dropped` / `language-coercion-check-dropped`：
  两个 YAML 装载点各自去掉共享检查（B1 的两个调用点分别钉住）。
- `object-tag-hardcoded` / `parse-cors-object-literal` / `js-string-object-literal` /
  `scalar-units-tag-dropped`：`object_tag`、`parse_cors_origins`、config `js_string`
  对象臂、`scalar_units` 四处重新各自硬编码 `[object Object]`（C1/C2）。
- `store-reload-guard-broadened`：store 的 reload 守卫放宽为不看 `error.name`，
  非 config 失败清掉保留诊断（A1/A2 的端到端后果）。

每个变异体只应用到原始字节（`content.replace(old, new, 1)`，锚点必须恰好出现一次），
目标测试必须先在未变异基线上绿；杀死判据是退出码 101、`negative_panic.named_test_killed`
在目标测试自己的 panic 载荷里找到该变异体对应的 `#1833` 断言消息。编译失败记 INVALID，
不算杀死。运行后逐个恢复原始字节并核对 SHA-256；`mutating_sources` 另对整棵 `src`
树做 HEAD 守卫（脚本本身随修复提交，未提交状态下本脚本会拒跑——在宿主提交后执行）。

复现（从仓库根，输出目录必须为新目录，独占本 worktree）：

```sh
CARGO_TARGET_DIR=/home/clawbox-08/ws/pdrs-worker-build/issue-1833 \
python3 -I src/pilotdeck-gateway/tests/fixtures/config-error-names/negative_controls.py \
  _tmp/20261112-issue-1833/impl-r1/negative-controls-host
```

可加位置参数只跑子集（名字取自 `EXPECTED_NAMES`）；未知名字、已存在的输出目录、
不可用的 `CARGO_TARGET_DIR` 一律 rc 2 拒跑（`negative_selection` 契约）。

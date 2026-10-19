# 权限决策场景夹具

`oracle.json` 来自固定 TS 基线（`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`）的真实运行：直接 import
`src/permission/decision/PermissionRuntime.ts`、`src/permission/policy/matchPermissionRule.ts` 和
`src/permission/protocol/types.ts`，用确定性假工具（`name` / `kind` / `isReadOnly` / 按次返回的
`checkPermissions`）驱动 56 个决策场景，另录 38 个 `matchPermissionRule` 场景、9 个
`isPermissionMode` 值和 4 个 `createDefaultPermissionContext` 结果。

## 来源与守卫

生成器先过共享溯源守卫 `tests/baseline_source_guard.mjs` 的 `bindSourceTree`：显式绝对路径 Git、
HEAD 等于固定 commit、整个已跟踪 `src/` 逐文件对照该 commit 的 blob；另外前后两次断言
`git status --porcelain --untracked-files=no` 为空。`sourceSha256` 是三个入口的相对 import
传递闭包（650 个文件，含 `import type` 的传递依赖），每个文件都对照 `git show <commit>:<path>`
的字节。locale 断言 `Intl.Collator().resolvedOptions().locale === "en-US"`。

场景使用合成 POSIX 路径（`/ws/project`、`/ws/extra`、`/ws/project/.pilotdeck/plans`），
决策引擎的路径处理是词法 `node:path`（不触达文件系统），因此 oracle 可移植、每轮不写磁盘。
生成器每次在输出父目录下 `mkdtemp` 独占子目录，`flag: 'wx'` 写文件；oracle 写盘前断言不含
基线路径、输出父目录和临时根目录。

## 覆盖

- 规则匹配：精确名、通配前缀/后缀/多星、大小写敏感、空规则集、空 pattern、未知工具上
  pattern 被忽略、`write_file`/`edit_file` 无 pattern 时的 workspace 归属（cwd、附加根、
  `..` 词法折叠、越界拒绝）、`send_attachment` 路径 pattern、bash 的 `:*` 归一化、命令 trim、
  非字符串命令、换行边界（`.*` 不跨 JS 行终止符）、路径反斜杠归一化、NUL 拒绝、
  `file_path ?? filePath` 的空值回退（非字符串不回落）、UTF-16 lone surrogate、正则元字符按字面量。
- 决策优先级：deny → ask → session allow → acceptEdits 安全编辑 → allow → tool
  `checkPermissions` → 模式兜底；`session allow` 只压过 user deny；policy ask 在 bypass 下仍保留；
  policy/ask 与 `canPrompt`/`policyCanPrompt` 的 deny 文案；force 仅对 session 的 `mcp__*` 生效；
  plan 模式对只读、plan 目录 markdown、bash 拒绝文案（120 UTF-16 单元截断，含拆开代理对）；
  bypass 覆盖 tool ask；tool 返回 allow/deny/cancel/ask/passthrough/未知类型的归一化；
  `updatedInput` 原样保留；`inputSummary` 的 `JSON.stringify`、500 单元截断和 `undefined` 回退。
- 负控：`wildcard-case-mismatch` / `file-pattern-case-sensitive` 钉住大小写敏感（改成大小写
  折叠即与 oracle 不符）；`user-deny-beats-user-allow` 同时命中 allow 与 deny，oracle 必须是
  deny（把 allow 提前即不符）。Rust 侧另有同名测试固定这两条。

## 范围与已知边界

- 只移植决策纯函数：`protocol/types.ts` 的常量/上下文、`policy/matchPermissionRule.ts`、
  `decision/PermissionRuntime.ts`。`settings.ts` 的读写属于本地文件持久化，不在此 PR；
  规则由已合并的 A6.3-pr6 `config::permission` registry 提供（`PermissionRuleSet`），
  其对外形状未改。
- 权限模式以 TS 为准：`PERMISSION_MODES` 只有 `default` / `plan` / `bypassPermissions`；
  SDK 的 `acceptEdits` 不是第四个模式，而是 `PermissionContext.acceptEdits` 布尔适配位
  （基线在 `cli/SessionAgentConfigBundle.ts:182` 由 `sdk.permissionMode === "acceptEdits"`
  置位），Rust 侧同样按上下文标志处理，只放行 workspace 内三个文件编辑工具。
- `tool.checkPermissions` 的完整运行上下文由后续宿主适配层持有；本引擎只把
  `PermissionContext` 交给注入的 `PermissionTool`。
- `wildcardToRegExp` 在基线里会转义全部正则元字符、只把 `*` 变成 `.*`，因此这里用等价的
  字面量分段 + 单星匹配实现，不编译正则：大小写敏感、`.` 不跨 `\n`/`\r`/U+2028/U+2029、
  `^...$` 只匹配完整输入。
- `summarizeInput` 的 `JSON.stringify` 复用 `pilotdeck_protocol::stringify_json`（迭代实现）。
  V8 在约 6855 层嵌套对象/数组时抛 `RangeError`，而 Rust 不设该递归上限；输入深于该实测
  边界时 TS 记 `[unserializable input]`、Rust 仍能序列化并截断，方向是更宽松，属已知偏离。
  `String(input)` 回退复用 `crate::js_value::to_string_units` 的 V8 深度上限（2000 层）。

## 复现

从仓库根运行（Node 22，使用基线现有 tsx，不安装依赖）：

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
scratch=$(mktemp -d "${TMPDIR:-/tmp}/pdrs-permission-verify.XXXXXX")
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/permission/generate.mts "$scratch"
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/permission/generate.mts "$scratch"
# 两次运行的 oracle.json 逐字节一致
find "$scratch" -name oracle.json | sort | xargs cmp
```

更新正本时把新生成目录的 `oracle.json` 覆盖到本目录，并保留上面的两次一致性核对。

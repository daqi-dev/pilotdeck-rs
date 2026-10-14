# A4 文件类 TS golden 与 Node 实测

基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，只读来源 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck`。Node v22.22.0 / ICU 78.2，实际默认 locale 为 en-US。不启动服务、不使用真实用户 home。

## 原件与来源

- `cases.json`：60 条真实 GatewayWsConnection request/response 原始 JSON 字符串，以及独立读取的文件系统输入快照。全部 60 条在 Rust 单测逐字节对比（不改 ID、路径、字段顺序、时间或内容），包括 4 条 security-comparison：根外绝对路径和根外 `..` 拒绝，根内绝对路径和根内 `..` 成功。第 2 轮撤回上一轮一律拒绝的要求；files-57/58 的 TS 成功原件保留，作为上一轮错误收紧的真实反例。
- `race-probes.json`：第 2 轮新增的 4 条真实 TS 检查后替换探针（叶子/父目录 × realpath 后/stat 后），全部读出隔离临时树的 `OUTSIDE_SECRET`；另含 4 条孤立 UTF-16 surrogate 路径请求/响应字符串，确认 Node 文件名编码与回传原文。
- `node-runtime.json`：Node readdir 与 localeCompare 的实际顺序、stat 的 size/mtime/birth/ctime、UTF-8/BOM/无效字节/截断/base64、resolve 与 realpath/Dirent/stat 的符号链接行为；另含 7 组环境变量 locale 优先级及排序/大小写探针、maxBytes 强制转换结果。
- `../../record_project_files.mts`：可重跑的离线 oracle。列表调用真实 `InProcessGateway.prototype.projectFilesList`，使用真实 `listWebProjects` 和项目 registry。文件读取直接从基线 `createLocalGateway.ts` 提取实际注入 callback，Node 内置 TypeScript stripper 仅擦除类型；依赖注入真实 Node fs/path、真实 registry/pathSafety/DialogGatewayError，非手写读取替身。原 callback 的 SHA256 在 runtime 原件中。

全部路径/时间来自同一次生成。快照保留 UTF-8 与二进制的原始 base64、symlink target、Dirent.isDirectory 与 Node stat 数字。默认测试重放这些输入，因此不需要还原历史绝对路径，也不依赖 Node 或基线安装；响应字节不做归一化。额外的 `PDRS_FILES_NATIVE_CHECK=1` 在生成目录仍存在时用 Rust 生产文件系统适配器逐字节重放全部 60 条响应，独立验证 stat 数值与真实读盘。

## 回源结论

以下行号相对基线 `src/`：

- WS 分发：`gateway/server/GatewayWsConnection.ts:363-364,399-401`；正常响应及结构化错误包装：`:187-252`。
- 列表项目注册校验：`gateway/client/InProcessGateway.ts:2165-2174` → `cli/GatewayDialogBundle.ts:49-65,79-84` → `web/server/listProjects.ts:2-31`，仅磁盘状态，不读取会话或连接内存。
- 列表实现：`gateway/dialog/projectFiles.ts:29-59,61-179`。根 resolve/realpath/stat；隐藏文件保留，`.gitignore` 不参与；仅真实目录排除 `.git/.pilotdeck/.tmp/node_modules/dist/build/coverage`。两次 localeCompare 排序。符号链接根内跟随，根外/断链跳过；输出 canonical relativePath，文件 alias 可重复，目录 visited 去重递归。无文件大小或二进制过滤。100000 项（含排除/消失项）、64 深度、5 秒上限。目录 size=0，文件 size=stat.size，mtimeMs 为 Node 原值。
- query：trim/toLocaleLowerCase，UTF-16 长度最多256，name（原 child 名）与 relativePath 非重叠匹配，UTF-16 范围。limit 默认100、1..500整数。cursor/ID 的 JSON 字段次序、SHA256、base64url 都原样复现。见 `projectFiles.ts:111-179`。
- 正式文件读取：`cli/createLocalGateway.ts:1098-1114`；不要误用 `InProcessGateway.ts:2259-2277` fallback。registry 允许 General（PILOT_HOME），见 `gateway/dialog/projectRegistry.ts:27,41-59`。路径检查 `tool/builtin/filesystem/pathSafety.ts:113-116` 也拒绝 `..secret` 名字前缀，Rust 保留该行为。
- 读取不存在路径/断链/根外目标：`PATH_NOT_ALLOWED`；目录/FIFO：null；无隐藏或二进制过滤。maxBytes 默认1000000，范围1..10000000，字节截断后转 base64 或容错 UTF-8（保留 BOM）。Rust 以8 KiB块增量读取，最多消费 maxBytes，不按 stat.size 预分配，也不读取文件余下部分。
- 读取安全：对齐 `resolve(root, input.path)` 后检查，接受根内绝对路径和含 `..` 的路径，响应保留输入 path 原文；归一化越界仍按 TS 错误形状拒绝；根内链接保留支持。realpath 后使用描述符逐组件 NOFOLLOW（包含 canonical root 的祖先），防止检查后替换链接逃逸；目录遍历使用 search 权限（macOS O_SEARCH / Linux O_PATH）。

错误 golden 覆盖 PROJECT_NOT_FOUND、INVALID_QUERY、INVALID_LIMIT、INVALID_CURSOR、PATH_NOT_ALLOWED、FILE_INDEX_LIMIT。非字符串参数及 projectKey 等未覆盖的非法参数形状沿现有策略透传 TS。字符串 path（含孤立 surrogate）走本地归一化/根内验证：文件系统访问采用 Node 的 U+FFFD 替换编码，响应保留原始 UTF-16 字符串。已验证的正常请求均本地处理；未测的平台特有底层 IO errno 也保留 TS 处理，避免伪造不同错误诊断（常见 ENOENT/EACCES/ENOTDIR 已实现）。

## 复跑

从本仓根执行（复用只读基线已有 tsx；不需安装新 JS 依赖）：

```sh
node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/record_project_files.mts \
  /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  src/pilotdeck-gateway/tests/fixtures/project-files _tmp/node-files
PDRS_FILES_NATIVE_CHECK=1 CARGO_HOME="$PWD/_build/cargo-home" \
  cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway project_files
```

每次生成使用新的 mkdtemp；fixtures 会更新为该次真实路径/时间，因此需重新编译测试。第 2 轮实际输出位于 `_tmp/round2-oracle/`，入库仅新增 race-probes.json，原 cases.json 与 node-runtime.json 保持上一轮原件不变。竞态探针恢复自己创建的目录，保留普通复测树可用。每次测试自己的临时树由 RAII 清理；oracle 的树保留用于原生复测。该证据不等同于正式 gateway 场景对拍，也不证明独立 upstream observer 中请求缺席。

## 安全差异登记候选（状态：待批；尚未修改冻结尺子）

原“绝对路径 / `..` 一律拒绝”候选撤销：这是正常 TS 行为，不是 bug，不应存在行为差异或比较豁免。

保留候选：`project_file_read` 的检查后符号链接替换逃逸。类别为 `.42cog/intent.md`「默认修 bug」第③项安全问题，状态 **待批**。

来源（相对基线 `src/`）：`cli/createLocalGateway.ts:1105-1107` realpath 后检查根内，`:1109` 随后按路径 stat，`:1111` 再按路径 readFile；没有钉住文件描述符，也没有 NOFOLLOW。`tool/builtin/filesystem/pathSafety.ts:113-116` 只检查路径关系，不封闭检查与打开之间的时间窗口。

`record_project_files.mts` 在原 callback 的真实 realpath/stat 完成后替换本次临时树的叶子或父目录，再继续调用真实 fs.stat/fs.readFile。4 种情况都成功返回根外的 `OUTSIDE_SECRET`，证据见 race-probes.json。该字符串来自探针自己创建的假数据，不读取用户文件。callback SHA256 与 node-runtime.json 相同。

Rust 对应测试 `checked_path_replacements_cannot_read_outside_or_fall_back_to_ts` 走 dispatch + 原生 Files：在 realpath 后替换时本地返回 null（stat 不再是普通文件或无法访问）；在 stat 后替换时 openat/NOFOLLOW 本地报错（叶子通常 PATH_NOT_ALLOWED/resolves outside，父目录按系统 ELOOP/ENOTDIR 诊断），均不返回根外内容、不透传 TS。普通根内链接先 canonical 后正常读取。

仅此已证实的竞态属于安全差异登记候选；4 条普通路径对照全部匹配 TS。`specs/gateway-parity/accepted-diffs.md` 已冻结，协调者按独立审批/登记流程处理，本 PR 不编辑它、不修改比较器、不添加正常响应豁免。

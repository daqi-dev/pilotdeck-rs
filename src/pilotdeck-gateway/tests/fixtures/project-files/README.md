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

## 第 4 轮：V8 切片表示与读取错误优先级

新增 `node-runtime-round4.json`，独立录制器为 `../../record_project_files_round4.mts`；前三轮 fixture 原件不变。录制器使用真实 `InProcessGateway.projectFilesList`、生产读取 callback 与 `GatewayWsConnection`，离线运行，不启动服务。证据记录 Node **v22.22.0** / V8 **12.4.254.21-node.33** / ICU **78.2**、`LANG=tr_TR`（resolved locale `tr-TR`）、非 root uid、源码 SHA256 和文件输入快照。

- 26 条列表 request/response 原始字符串：`FILEFILEFILEFILE` 有无 trim、12/13/14 字符的顶层与嵌套名称、仅前端 BOM / 仅后端 tab / 双端空格 trim、dotless `ı` 查询；另覆盖分页与 trim 前后 cursor 签名。嵌套 symlink 指向顶层项，确认切片判断依据 canonical relativePath，而非遍历深度。Rust 在独立 locale 子进程中逐字节比较全部响应，不修改测试主进程环境。
- Node 结果：`" FILEFILEFILEFILE "` 对 `nested/FILEFILEFILEFILE.txt` 返回空数组；无空格则命中 name 和 relativePath。未 trim 的 `I` 重复查询对顶层 12 字符项命中两字段，13/14 字符项只命中 name；嵌套项仍命中两字段。trim 后长度 12 沿用 root 小写，长度 13/14 改为土耳其语小写，仅命中顶层 relativePath。对应 cursor 在 12 字符时可复用于无空格查询，13/14 字符时返回 INVALID_CURSOR。
- 同一个 `size=2147483648, blocks=0` 稀疏文件：chmod 000 返回 `EACCES / permission denied, open ...`；恢复 0600 后返回 `ERR_FS_FILE_TOO_LARGE / File size (2147483648) is greater than 2 GiB`。Rust 原生 dispatch 测试对照完整响应，仅替换测试临时根路径；root CI 可跳过无法触发的 EACCES，本轮非 root 两种状态均实际执行。Rust 在报大小错误前经 NOFOLLOW 安全打开并请求零字节前缀，不读取大文件内容。

**已知风险：此行为依赖 V8 内部字符串表示，不是 ECMAScript/Unicode 的稳定保证；引擎升级可能改变。** 第 4 轮按基线实际构造方式推断：trim 确实移除字符，或 canonical relativePath 位于顶层，且剩余 UTF-16 长度 ≥13 时为切片；Latin-1 非切片才走 root 小写。此轮实测的 Latin-1 根下，name 与经 join 重建的嵌套路径保持非切片规则；双字节根的补充与最终残余风险范围见下节。

复跑（输出到临时目录，审阅后仅复制新的 round4 fixture）：

```sh
LANG=tr_TR env -u LC_ALL -u LC_MESSAGES -u LC_COLLATE -u LC_CTYPE \
  node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/record_project_files_round4.mts \
  /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  _tmp/20261014-a4-files-round4/node-runtime-round4.json \
  _tmp/20261014-a4-files-round4/scratch
```

## V8 字符串表示依赖

第 5 轮按协调者决定登记为**已知残余风险（自决·待复核 / AI 代批，待人复核）**：V8 是否走 Latin-1 小写快路取决于字符串的内存表示，不能仅凭字符内容穷举。本轮补齐双字节父串样本后，不再追补仅在 `tr` / `az` / `lt` 等具有特殊大小写规则的 locale 下才可观察的同类表示差异；**其余情形不保证与 Node 一致**。此范围限于表示导致的 locale/root 小写分歧，不扩大到普通匹配、排序、路径安全等其他行为，也没有修改冻结尺子或添加比较豁免。

目前已对齐的三条规则（阈值均按 UTF-16 单元计数）：

1. **Latin-1 非切片快路**：内容全部 ≤0xFF 且未触发下述表示规则时，用 root 小写；内容含 >0xFF 字符时用 locale 小写。第 3 轮 `node-runtime-round3.json` 的 `localeProbe` 实测包括 `tr` 下 `FILE.txt → file.txt`、`FILE😀 → fıle😀`，以及 `lt` 下 `Ì → ì`、`Ì😀 → i̇̀😀`。Dirent.name 继续遵循这条规则。
2. **长切片**：query 的 trim 确实移除字符且结果长度 ≥13，或 canonical relativePath 位于顶层且长度 ≥13 时，按 locale 小写；长度 12 不触发。第 4 轮 `node-runtime-round4.json` 的 26 条生产列表响应覆盖 12/13/14、首尾 trim、嵌套到顶层的 symlink、匹配范围与 cursor 签名；Latin-1 根下的嵌套路径仍按第 1 条处理。
3. **双字节父串**：canonicalRoot 含 >0xFF 字符且 canonical relativePath 任一段长度 ≥13 时，整个 relativePath 按 locale 小写，包含嵌套路径。第 5 轮 `node-runtime-round5.json` 的 20 条生产列表响应覆盖中文根、Latin-1 `é` 根、ASCII 根，12/13 边界，长段位于叶子/父目录/中间目录，以及 ASCII 项目别名指向中文 canonicalRoot。

第 5 轮实测环境为 Node **v22.22.0** / V8 **12.4.254.21-node.33** / ICU **78.2**，`LANG=tr_TR`，resolved locale **tr-TR**。录制器 `../../record_project_files_round5.mts` 离线调用钉死基线的真实 `InProcessGateway.projectFilesList` 与 `GatewayWsConnection`，保存 request/response 原始字符串、独立文件系统输入快照与列表源码 SHA256（`b004f655300aa2e67b6643cd46d5effa221b93f2efe253d3847bd20890bfd1f1`）。没有用重写的小写算法生成期望值，也没有直接检查 V8 内部对象；上述表示解释是对实测行为的有限建模。

| 第 5 轮 `LANG=tr_TR` 样本 | Node 的 relativePath 匹配证据 |
|---|---|
| 中文根，`a/b/` + 13 个 `I`；13 个 `I` + `/x`；`a/` + 13 个 `I` + `/z` | 命中 13 个 dotless `ı`，不命中 ASCII `i`。叶子/目录的 name 仍按 root 小写。 |
| 中文根，上述长段改为 12 个 `I` | 命中 12 个 ASCII `i`；即使路径总长超过 13，也不触发嵌套路径规则。 |
| 中文根，13 个 `A` + `/I` | 末尾一单元 `I` 在 relativePath 中命中 `ı`、在 name 中命中 `i`，证明影响整个拼接路径；12 个 `A` 的对照两字段都命中 `i`。 |
| `é` 或 ASCII 根，同样的嵌套 12/13 样本 | relativePath 命中 ASCII `i`；顶层 13 单元目录仍按第 2 条命中 dotless `ı`。 |
| ASCII 项目别名 → 中文根 | 与中文 canonicalRoot 的响应行为一致，判断不能使用原始 projectKey。 |

Rust 测试 `node_round5_two_byte_parent_list_responses` 在独立 locale 子进程中经 dispatch 逐字节重放全部 20 条响应（含 ID、字段顺序、mtime、路径与 matches），不归一化响应；修复前该测试失败。输入快照额外保存独立 stat 的 `mtime_ms_exact` 十进制字符串，由 Rust `f64::from_str` 精确还原，避免 serde_json 默认浮点解析产生 1 ULP 偏差；响应原文不改。快照适配器也修正根 symlink 的空 suffix，避免人为添加 realpath 没有的尾斜杠。第 3/4 轮证据与回归保留。第 5 轮没有重新穷举 `az` / `lt`，也不声称全 Unicode、任意字符串构造方式或其他 Node/V8 版本等价；引擎升级需重跑实测。

复跑（scratch 的完整绝对路径须只含 Latin-1 字符，以保证 ASCII / Latin-1 根对照不受祖先污染）：

```sh
LANG=tr_TR env -u LC_ALL -u LC_MESSAGES -u LC_COLLATE -u LC_CTYPE \
  node --import /Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/record_project_files_round5.mts \
  /Users/da/ws/pilotdeck-rs/resources/PilotDeck \
  _tmp/20261014-a4-files-round5/node-runtime-round5.json \
  _tmp/20261014-a4-files-round5/scratch
```

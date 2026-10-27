# SkillManager TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测源码 `src/extension/skills/SkillManager.ts`、`types.ts`，绑定选择取 `src/composition/types.ts` 与 `src/cli/createLocalGateway.ts`（SHA-256 记在 `oracle.json`）。所有步骤结果与文件树都由真实 `SkillManager` 产生，无实现源码复制；环境准备（Node v22.22.0、基线 lockfile 的 tsx、`PDRS_BASELINE`）同 `../operation-ledger/README.md`。`localeCompare` / `toLocaleLowerCase` 取 ICU 默认 locale，生成器断言为 `en-US`，Rust 测试固定同一 locale。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/skills/generate.mts src/pilotdeck-gateway/tests/fixtures/skills
```

`oracle.json.scenarios[]`：`options`（builtin 是否配置、isVirtualProjectRoot 的 env、generalCwdPaths）、`setup`（起始文件树：目录、文本/base64/定长稀疏文件、符号链接、批量文件）、`steps[]`（list/read/write/create/delete/validate 的输入与 TS 结果）、`tree`（步骤后整棵文件树：每个文件的大小与 SHA-256，≤4 KiB 的另记 UTF-8 文本或 base64）。生成器对每个场景逐步断言结果类别（技能名、条目数与是否有 nextCursor、validate 的 hardFails|warnings 码序列、错误码），最后断言下列错误码与校验码全部至少出现一次：`not_configured project_required read_only invalid_slug invalid_input not_found conflict ENOTDIR EISDIR EEXIST TypeError ERR_INVALID_ARG_TYPE` 及 validate 的 16 个 issue 码。

Rust 默认测试 `real_ts_scenarios_match_outcomes_and_file_trees` 逐场景在新目录重建 `setup`，重放全部步骤，结果按紧凑 JSON 逐字比较，最后文件树逐字节（大小 + SHA-256 + 文本）比较。

与机器无关的归一化：
- **场景根目录 → `<dir>`**：输入中的 `<dir>` 替换为场景根；输出（结果、错误文案、validation）中的场景根替换回 `<dir>`。`~` 展开所用 HOME 两侧都设为场景根。
- **`mtime` → `"<mtime>"`**：生成时刻不可复现；交叉测试保留真实 mtime 并逐值比较。
- **`nextCursor` → `{raw, offset, signature}`**：游标是 base64url(JSON)，签名内含 projectKey（机器路径）。签名不含路径时 `raw` 保留原字节逐字比较，否则为 `null`、只比较解码后的 offset 与归一化签名。输入里的 `<cursor:K>` 表示「第 K 步返回的游标」，两侧各用自己的游标续页。
- **TypeError 只比 name 与 code**：V8 的 TypeError 文案不在契约内；`SkillManagerError` 与 Node fs 错误（`ENOTDIR: not a directory, access '<dir>/…'` 等）的文案逐字比较。

| 场景 | 覆盖 |
|---|---|
| list | builtin/user/project 三层覆盖标记、无效 slug/普通文件/空目录/SKILL.md 为目录/目录符号链接/断链、ICU 排序（`_ - .` 大小写与数字）、general chat（pilotHome、空 projectKey 的回显怪癖）、scope 过滤、query 去空白与小写和匹配区间、游标翻页与签名不符、scope/query/limit/cursor 每条 invalid_input |
| list-scandir-error | user 根是文件：list/read/write/create/delete 各自的 Node 错误（scandir/open/mkdir/access/lstat） |
| no-builtin | 未配置 builtin：not_configured、builtin 写为 read_only、slug 先于 scope 校验 |
| read | 无/未闭合/CRLF/紧贴/兼容回退/重复键/数组/标量/非字符串字段/自引用 alias/BOM/无效 UTF-8/`.inf` 版本/孤立代理的 frontmatter；builtin 只读标记、project 尾斜杠、plugin/缺省 scope；not_found、EISDIR、ENOTDIR、9 种非法 slug、100 字符上限、project_required 三种、非字符串 projectKey、null 输入 |
| virtual-roots | 桌面运行时根（env 指定与 `/resources/runtime` 后缀）、自定义 generalCwdPaths、pilotHome |
| write | builtin 只读先于内容检查、内容检查先于 slug、空内容、孤立代理写成 U+FFFD、CRLF、非字符串内容、EEXIST（文件挡路）、EISDIR（SKILL.md 为目录） |
| create | 字段生成（换行折成空格、去空白、空 name、null 描述/正文）、空白 content 回退字段、完整 content、conflict（目录或文件已存在）、ENOTDIR、非字符串 name/body 的 TypeError（目录已建、留空）、project 范围、孤立代理 |
| delete | 递归删除、重复删除、删除符号链接不碰目标、删除普通文件、read_only、invalid_slug、project_required、ENOTDIR |
| validate-disk | 合格包（大文件、符号链接、可执行扩展名大小写、`.sh` 点文件无扩展名、嵌套、非 ASCII 名）、描述过短/过长、缺 name/description、无 frontmatter、空 SKILL.md、无 SKILL.md、源是文件/不存在、兼容回退、单文件超 10 MiB、总量超 50 MiB、超 500 文件（计数在第 501 个停）、去空白、`~` 与 `~/`、`..`、源是符号链接、空白路径、空对象、null、非字符串 sourcePath 落到 manifest |
| validate-manifest | unsafe_path（`..`、绝对路径）、跳过非字符串/空 relativePath 与非对象条目、`Number(size) || 0`（字符串、十六进制、非数、数组、科学计数、布尔、Infinity）、扩展名取自整个相对路径、空列表、缺/非字符串 skillMdContent、BOM、超 500 条、总量、`files` 含 null（TypeError）、`files` 非数组、空白 name/description、UTF-16 长度 19/20 与 1024/1025 边界、整数键序与自有 `__proto__`、兼容回退块标量、alias、嵌套值、数组输入；上限等值点（单文件正好 10 MiB 与多 1 字节、正好 1 MiB、总量正好 50 MiB 与多 1 字节、正好 500 条） |
| import | copy（fs.cp：相对符号链接改写成绝对路径）与 symlink 两种模式、force 覆盖、slug 去空白与回退 basename、project 范围建根、`~`、源是目录符号链接（copy 出一个链接）、目标是断链（copy 报 ERR_FS_CP_DIR_TO_NON_DIR，symlink 报 EEXIST）、包内 FIFO（ERR_FS_CP_FIFO_PIPE，残留随后删除）、目标在源内（ERR_FS_CP_EINVAL）、self_import、source_missing、source_not_directory、ENOTDIR、no_skill_md、SKILL.md 为目录与无 frontmatter（validation_failed 带 validation）、非法 slug、非字符串 slug、builtin、缺/空白/非字符串 sourcePath、project_required、null 输入 |
| import-unknown-type | 在指定文件的 lstat 分类上隐藏 isFile，真实 `SkillManager.import → fs.cp` 生成 `ERR_FS_CP_UNKNOWN`；删除部分复制树后再次 import 成功，证明钩子已恢复 |
| import-symlink-to-subdirectory | 源链接指向子目录，目标断链含不存在的 `missing/..`；OS access 失败，但 cp 将目标 readlink 词法解析为源目标祖先，真实产生 `ERR_FS_CP_SYMLINK_TO_SUBDIRECTORY`，不需要竞态注入 |
| scan-unreadable-child | 可读 → 子目录 chmod 000 → 恢复 0755；原生 readdir 先证实 EACCES，scan 对整个技能返回 fileCount=0、totalSize=0，元数据保留；恢复后全字段与原值一致 |
| scan | 子目录排序（有 SKILL.md 在前，再 localeCompare）、跳过文件与断链、目录符号链接、SKILL.md 为目录（name 为 null、0 文件）、递归计数跟随链接（文件链接计入、FIFO 不计、自指链接展开到 ELOOP）、父目录本身是技能、去空白与尾斜杠、`~`、not_directory、not_found、ENOTDIR、invalid_input 三种、null |

`oracle.json.bindings[]`：createLocalGateway.ts:853–860 的选择表，由真实 `isDisabledModuleBinding` / `isExternalModuleBinding`（`src/composition/types.ts`）判定，含 null/原始值在 `"implementationId" in` 处的 TypeError。`oracle.json.disabledError`：`createDisabledSkillManagementPort` 是模块私有函数，生成器从固定源码中抽取它唯一的错误（并断言八个方法都指向它），不是运行时结果。FIFO 由 `mkfifo` 创建，文件树记作 `kind: "fifo"`，不读取内容。

#241 / #251 新场景的 `steps[].unknownType` 只对一个真实普通文件启用分类钩子；不是未知 inode 的宿主实测。TS 不复制 cp 实现、不手写错误，Rust 只在 `cfg(test)` 下遮蔽同一路径的 `FileType.is_file`，两侧真实 import 错误的 name/code/message 与整棵文件树均逐字比较。钩子必须命中，TS `finally` / Rust 线程局部 Drop 恢复；正常构建没有该钩子。生成器必达码表包含两条新增 cp 码，Rust 必需场景清单要求三场景及全部恢复步骤。

`steps[].denyReadDir` 在单步执行前 chmod 000、执行后恢复 0755，快照和清理只在恢复后进行。生成器若原生 readdir 仍可读（root 或权限模型不支持），打印 `NOT_RUN` 原因并断言失败，拒绝覆盖完整 oracle。Rust 以原生 `read_dir` **完整 collect** 探测；只有原生仍可读才打印 `NOT_RUN` 并跳过拒读格，前后健康格及其完整金样仍必须通过；未知 errno 直接失败。仅此格跳过不算真实权限验收通过。

`negative_controls.py` 只暂改夹具（不改产品源码），串行运行 16 个 Rust 回放负控与 3 个真实 TS 生成器负控：两条 cp 的 name/code/message、拒读格 count/size、三个必需场景及其步骤、钩子未命中、移除拒读条件。每个变异必须出现对应断言失败，随后恢复重放、重生成 oracle 逐字节一致。原始输出分别独占写入 `--out`；没有真实 EACCES 时明确失败而不计权限负控通过。

```sh
PDRS_BASELINE=/absolute/path/to/PilotDeck python3 \
  src/pilotdeck-gateway/tests/fixtures/skills/negative_controls.py \
  --out _tmp/20261026-issue-241/negative-controls
```

`generate.mts <root> --verify-rust read|mutate` 由 Rust 忽略测试 `real_ts_and_rust_read_each_others_skill_trees` 调用：Rust 先 create/write（含孤立代理）一棵树，真实 TS 读取（list/read/validate）并输出结果，Rust 在同一棵树上重放并逐值比较（含真实 mtime）；随后 TS create/write/delete，Rust 读取 TS 写出的结果并与 TS 自己的读数逐值比较。

```sh
cd src
PDRS_BASELINE=/absolute/path/to/PilotDeck cargo test -p pilotdeck-gateway skills -- --include-ignored
```

范围：证明 SkillManager 八个方法的契约（结果对象、错误类与码/文案、磁盘文件树字节）与 skills 绑定选择；默认测试经 `SkillManager::call` 重放，因此每个方法的 errno 准入表（`node_io::LOCAL_ERRNOS` 的 `skill_*`）必须恰好等于该方法在 oracle 中出现过的 errno。external 绑定的成功/未配置/provider 报错三类由 `../skills-external/` 单独录制（真实 `createSkillManagementPort`）；本 fixture 的 `bindings[]` 只证明选择谓词。未接入 gateway method，未宣称本地化。已知差异：frontmatter 落入兼容回退时孤立代理先变 U+FFFD（TS 保留）；frontmatter 含自引用 alias 时 validate 在 Rust 报 TypeError（TS 返回带环对象，随后在 JSON 传输时失败）；Rust 不读进程 cwd 之外的 `os.homedir()`，由 `SkillManagerOptions::home_dir` 或 `$HOME` 提供；深嵌套数组的 RangeError 阈值是 Rust 常量 `js_value::MAX_ARRAY_DEPTH`，与 V8 栈深不逐值相等。

## 平台相关行为（不进夹具）

scan 的递归 readdir 会沿目录链接展开。自指链接（`dir/loop -> .`）要一直展开到内核的 ELOOP 才停，所以文件数取决于内核的符号链接嵌套上限：macOS 是 32，Linux 的 MAXSYMLINKS 是 40。TS 本身在不同平台上的结果就不一样，而夹具要求与机器无关，CI 跑在 ubuntu，所以这类场景不录进 oracle（A6.8-pr4 四角色闸门 P1）。Rust 与 TS 在同一平台上的展开方式一致，都是按 metadata 能解析的深度展开。

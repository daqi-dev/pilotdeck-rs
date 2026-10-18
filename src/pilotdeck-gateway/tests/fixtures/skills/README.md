# SkillManager TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，被测源码 `src/extension/skills/SkillManager.ts` 与 `types.ts`（SHA-256 记在 `oracle.json`）。所有步骤结果与文件树都由真实 `SkillManager` 产生，无实现源码复制；环境准备（Node v22.22.0、基线 lockfile 的 tsx、`PDRS_BASELINE`）同 `../operation-ledger/README.md`。`localeCompare` / `toLocaleLowerCase` 取 ICU 默认 locale，生成器断言为 `en-US`，Rust 测试固定同一 locale。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 LC_ALL= node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
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
| validate-manifest | unsafe_path（`..`、绝对路径）、跳过非字符串/空 relativePath 与非对象条目、`Number(size) || 0`（字符串、十六进制、非数、数组、科学计数、布尔、Infinity）、扩展名取自整个相对路径、空列表、缺/非字符串 skillMdContent、BOM、超 500 条、总量、`files` 含 null（TypeError）、`files` 非数组、空白 name/description、UTF-16 长度 19/20 与 1024/1025 边界、整数键序与自有 `__proto__`、兼容回退块标量、alias、嵌套值、数组输入 |

`generate.mts <root> --verify-rust read|mutate` 由 Rust 忽略测试 `real_ts_and_rust_read_each_others_skill_trees` 调用：Rust 先 create/write（含孤立代理）一棵树，真实 TS 读取（list/read/validate）并输出结果，Rust 在同一棵树上重放并逐值比较（含真实 mtime）；随后 TS create/write/delete，Rust 读取 TS 写出的结果并与 TS 自己的读数逐值比较。

```sh
cd src
PDRS_BASELINE=/absolute/path/to/PilotDeck cargo test -p pilotdeck-gateway skills -- --include-ignored
```

范围：只证明 SkillManager 的 list/read/write/create/delete/validate 契约（结果对象、错误类与码/文案、磁盘文件树字节）。`import`、`scan` 与 disabled/external skills 绑定选择（createLocalGateway.ts:851–860）留 A6.8-pr4；未接入 gateway method，未宣称本地化。已知差异：frontmatter 落入兼容回退时孤立代理先变 U+FFFD（TS 保留）；frontmatter 含自引用 alias 时 validate 在 Rust 报 TypeError（TS 返回带环对象，随后在 JSON 传输时失败）；Rust 不读进程 cwd 之外的 `os.homedir()`，由 `SkillManagerOptions::home_dir` 或 `$HOME` 提供。

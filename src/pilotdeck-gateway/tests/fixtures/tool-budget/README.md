# tool-result budget 金样

固定基线 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，入口：

- `src/context/budget/ToolResultBudget.ts`（预算判定、spill、预览、替换块）
- `src/context/DefaultContextRuntime.ts`（`applyToolResults`，427-473 行）

`sourceSha256` 是这两个入口的**完整相对 import 闭包**（含 `tokenizer.ts`、
`ToolResultSpillPort.ts`、tool-result 拍平、prompt 组装等）。生成前后用共享
`bindSourceTree` 绑定基线 HEAD 的整棵 `src` Git blob，并断言基线工作树干净。
locale 取 ICU 默认 locale，生成器断言为 `en-US`；Rust 回放固定同一 locale。

## 生成与回放

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 \
  node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/tool-budget/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/tool-budget
cargo test --manifest-path src/Cargo.toml --locked --offline -p pilotdeck-gateway --test tool_budget
```

录用前生成两次并逐字节比较两份 `oracle.json`，再覆盖仓库金样。生成器每次用
`mkdtemp` 独占临时目录，结束前删除自己创建的目录。

## 输入与断言

每个 case 一个独立 `ToolResultBudget`（等价于新会话 bundle），真实调用
`DefaultContextRuntime.applyToolResults`。`oracle.json` 对每一步保存：

- `input`：传给 `applyToolResults` 的完整对象（路径规范化为 `<fixture>`）；
- `outputJson`：真实 TS 返回值的 `JSON.stringify` 原字节（保留键序、孤立代理项
  `\udXXX` 转义与数字格式）；
- `filesJson`：该步之后 spill 目录树的 `JSON.stringify`（目录、文件大小、
  SHA-256、≤4096 字节文件的原文）。

Rust 回放按 `Scratch` 独占临时目录重建同一布局，逐步比较完整 `outputJson` 与
`filesJson` 的逐字节内容，条数精确断言（39 cases / 42 steps）。路径统一规范化为
`<fixture>`；`locale` 固定 en-US。

#638/#690 fix-r1 回放路径映射：Rust 平铺文件名使用 `context-<uuid>-` 前缀，预置的 session 文件也映射到对应前缀文件。回放将实际生成的文件名前缀映射回 TS 名，再比较完整 outputJson/filesJson；不删除任何目录项，不剥 context 子目录，按映射后的路径恢复 TS 字典序。其余字段与文件字节严格比较。差异登记见 `A7.4-TOOLBUDGET-CONTEXT-ISOLATION-001-R1`。

覆盖：

- 预算内不动、`maxResultSizeChars` 刚好等于 UTF-8 字节数、`maxResultSizeTokens`
  刚好等于真实 o200k token 数；
- 超字符预算与超 token 预算分别 spill；`.txt` / `.json` 扩展名；
- >40,000 字节的抽样 token 估算：50 KB 与 150 KB 混合 ASCII/CJK/emoji（6,000
  单元采样切点跨代理对），各自在精确估算值内联、估算值−1 spill；
- 多 tool_result 混合（内联、超限、message-level text、`isError` 保序）；
- tool_result 内嵌 image / pdf：超限者落盘并追加 `media_reference`，小图保留，
  `bytes` 覆盖、`pages`、`detail`、PDF 占位文本；`pages` 只随 pdf 输出（image
  携带的 `pages` 被丢弃）；超限 pdf 缺 `mimeType` 时正常替换（preview 为
  `undefined`，输出无 `mimeType` 键）；
- supplemental image / audio：顶层媒体改写、别名编号 0001/0002、TS map 丢额外字段；
- supplemental 缺 `toolCallId`：小消息正常通过；超限媒体在真正用到 id 时报
  `Cannot read properties of undefined (reading 'trim')`（惰性，与 TS 同控制流）；
- `turnId: null`：文本 tool_result（无论内联与否）在 `safePathPart` 处报
  `Cannot read properties of null (reading 'trim')`；未读取 turnId 的空 primary +
  小 supplemental 正常通过；
- 非字符串 id：`turnId` 为 number、主块 `toolCallId` 为 number（文本与超限媒体
  两条分支）时报 `value.trim is not a function`（V8 以 `safePathPart` 的参数名
  `value` 命名）；supplemental 的非字符串 `toolCallId` 小消息正常通过、超限媒体
  报同一条文案；
- 空 tool_result、assistant 消息不参与预算；小 image 缺 `mimeType` 时 TS 在尺寸
  早退后才读它，保持原块；tool_result 内 audio 不参与媒体预算；
- emoji / CJK / 代理对切断：head+tail 预览按 UTF-16 单元切尾，跨代理对产生孤立
  代理项；head/tail 重叠时 TS 输出负的 `omitted`（`[-500 bytes omitted]`）；
- 两种目录布局：`.../tool-results/session`（refs 为兄弟目录）与普通 `spill`
  （refs 在目录内）；`readFilePath` 相对 workspace root；
- 替换缓存：同一 turn/toolCallId 复用记录；不同 turnId 重新落盘并使用下一个别名；
- spill 原文件已存在：`wx` 保留原字节，别名复制原文件，预览仍描述本轮工具结果；
- 目标已有别名文件：`COPYFILE_EXCL` 返回 EEXIST 后顺延到 `result-0002`；
- 写失败：父路径是普通文件（`ENOTDIR`）与目录 chmod 0555（`EACCES`，非 root
  环境）。TS 的 `applyToolResults` 把异常转成
  `tool_result_persistence_failed` / severity=error，并原样保留 tool_result；
  primary 已成功、supplemental 失败时 primary 的改写保留（TS 先赋值后 `Promise.all`）。
- 安全负例（Rust 侧集成测试，不对拍 TS 的越界写）：`toolCallId` 路径穿越形状被
  TS 同款正则压平；`toolResultsDir` 自身是符号链接、或 root 下中间目录是符号
  链接时拒绝写入（EACCES），外部目录保持空；spill 目标已被预置为指向目录外文件
  的符号链接或硬链接时，别名复制以 `O_NOFOLLOW` 打开源并用句柄 `fstat` 要求
  `is_file && nlink == 1`，拒绝（EACCES）且不读取外部内容，外部内容不进入任何
  输出或 `refs/`。

## 切分点

`hydrateReferences` 不在本 PR：Rust 宿主还没有 artifact-storage 缓存，恢复流程
留给 storage owner。真实 provider 的媒体物化不在这里。

## 写入竞态回归（#344）

Unix 上原文件与别名目标都复用 `project_files::create_new_beneath`：目录逐级
`openat + O_NOFOLLOW`，末级 `O_CREAT | O_EXCL`、0600，打开后 fstat。
`tool_budget` 单元测试在 containment 后、open 前按目标路径触发钩子，把中间
目录换成根外链接；断言钩子确实执行、根外为空、错误与预置链接完全相同。
新增回归直接经生产 persist/alias 路径，以 project 为根，覆盖预置或竞态换入
`.pilotdeck` / `tool-results` 链接，以及文件创建前替换 session / refs 根。
目录缺失时用父句柄 `mkdirat(0700)` 再 NOFOLLOW 打开；测试拒绝链接时不在
项目外建目录。真实 TS 的 `project-layout-missing-parents` 金样从缺失
`.pilotdeck` 开始，复刻 recursive mkdir 的正常落盘。

负控：`timeout 580 python3 src/pilotdeck-gateway/tests/fixtures/tool-budget/negative_project.py`。
串行跑四条变异，使用本树 `src/target`；每个 cargo 子进程有期限且超时终止进程组，
`finally` 按原字节恢复并核 SHA-256。控制包括 spill 根基准回退、按路径 mkdir、
移除目录 NOFOLLOW、跳过 mkdir；编译失败、空匹配、超时均不算检出。
非 Unix 保持原有按路径 `create_new` 行为。

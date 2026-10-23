# 插件磁盘 loader 真实 TS 金样

基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`，Node `v22.22.0`，`LANG=en_US.UTF-8`。

`generate.mts` 在自建临时目录创建插件，通过真实的 discovery、manifest parser、PluginLoader、PluginRuntime 生成 `oracle.json`。不改基线、node_modules，也不接触用户 PilotDeck 目录。磁盘 JSON 插件只提供声明式贡献；放置会抛异常的 `index.js` 证明 loader 不执行它。程序化内置插件仍由调用者注入，本次不新增 JS 宿主或进程模型。

生成器先调用共用 `bindSourceTree`：固定 HEAD、验证全部 tracked src 与 Git blob 一致、拒绝源文件符号链接和仓内 Git，并额外检查 tracked tree clean。`sourceSha256` 覆盖入口相对导入闭包。路径仅把本次临时根替换成 `<root>`，不改排序、诊断或贡献值。所有 `value`、`error`、`discovered`、`skills`、`attempts`、`steps`、`active`、`catalog` 都来自实际 TS 返回值；Rust 不生成期望值。

从仓库根运行（将 `PDRS_BASELINE` 指向固定基线）：

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/plugin-loader/generate.mts /tmp/pdrs-loader-one
LANG=en_US.UTF-8 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/plugin-loader/generate.mts /tmp/pdrs-loader-two
cmp /tmp/pdrs-loader-one/oracle.json /tmp/pdrs-loader-two/oracle.json
shasum -a 256 /tmp/pdrs-loader-one/oracle.json /tmp/pdrs-loader-two/oracle.json
/Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test plugin_loader
```

场景包括：空目录；合法插件；缺字段、错误类型及 JSON 语法错误；可选字段宽松投影；默认与配置目录；Markdown 前言和嵌套命名；输出样式及仅输出样式加载；inline/file hooks；MCP/LSP（含 150 层嵌套配置、多服务器与配置内部的插入序、数字键的 JS 枚举序及 runtime 合并）；同名同来源与不同来源；内置技能；独立技能仅根 SKILL.md；内置插件磁盘重载/失败回退/禁用；磁盘插件不受 builtin 开关影响；不可读父目录/插件目录/Markdown 文件；目录与文件符号链接、断链；UTF-8 枚举顺序；失败 refresh 保留旧 generation 与 lease，删除则正常退休；用 Promise 屏障记录真实 TS 的退休 dispose 次数、lease release 失败和关闭等待中的 generation 发布。

权限夹具在 Unix 非 root 用户执行。Rust 测试通过独立原生 `read_dir` / `read` 确认 `chmod 000` 的拒绝效果，失败时不跳过、不算通过；Drop 只恢复并清理该测试创建的临时目录。生成前 `assertPortablePaths` 检查包含隐式父目录的所有夹具路径：拒绝大小写折叠或 NFC/NFD 规范化后相撞的兄弟名。排序场景使用 `Z-upper` / `z-lower` 等互不撞名的 10 项，保留大写、小写、数字、Unicode 相对顺序。执行 `node --test src/pilotdeck-gateway/tests/fixtures/plugin-loader/portable-paths.test.mjs` 可验证旧 Z/z 及规范化撞名均被拒绝。

实现负控（会临时改 Rust 源码；一次仅运行一份，确保工作区独占）：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/plugin-loader/negative-controls.py \
  _tmp/20261022-a63-pr7b/negative-controls
```

每次检查唯一替换锚点，先执行 cargo-slots，再编译并运行对应金样测试。必须出现实际 `test result: FAILED`，编译失败不算命中；每项用 finally 恢复原始字节，汇总含恢复后的 SHA-256。参数可追加 mutation-id 仅重跑指定项。首次正控与每项失败摘要、范围外缺陷、最终检查结果均见工作区 `_tmp/20261022-a63-pr7b/RESULT.md`。

MCP/LSP 从 manifest 到 LoadedPlugin 及 runtime 合并保持现有协议类型 `JsValue`，保留普通键插入序、数字键 JS 枚举序及嵌套值。显式栈深度检查遵守共用 `MAX_ARRAY_DEPTH=2000` 护栏；超界返回 `Maximum call stack size exceeded`，该区间不声称与 V8 无界 JSON.parse 等价。TS 的服务器对象以序列化字符串保存，Rust 对加载结果和 runtime 合并结果逐字节比较，避免两侧先经 serde map 排序掩盖分歧；150 层用例也防止 runtime 合并偷偷截断为 32 层。

顺序依据是 Node `fs.readdir` 使用 `uv_fs_scandir`，其 Unix 实现向 `scandir` 传入 `strcmp` 比较器，并非直接沿用文件系统 `readdir` 顺序。参见 [Node v22.22.0 node_file.cc:1797-1805](https://github.com/nodejs/node/blob/v22.22.0/src/node_file.cc#L1797-L1805) 与 [libuv v1.52.1 fs.c](https://github.com/libuv/libuv/blob/v1.52.1/src/unix/fs.c)。本机 opendir 与 readdir 的实际不同顺序、正则折叠仲裁、修复负控见 RESULT 所引 `fix2-runtime-probes.json` 等原始证据。

#484 补充金样使用独立 `generate-regressions.mts` → `regressions.json`，原 `oracle.json` 不改。生成命令与上面相同，只替换脚本名，并逐字节比较两次 `regressions.json`。生成器同样绑定固定源树和完整导入闭包；输入通过磁盘 manifest 进入真实 TS runtime，结果中的 `inputJson` / `resultJson` 保留孤立代理的 JSON 转义。

补充样本覆盖 2047/2048/2049 个 UTF-16 码元、CJK 跨字节边界、代理对在截断点两侧、孤立高低代理、ECMAScript trim、skills 的首次插入位置和各级优先覆盖，以及目录别名的 TS 原始行为。`plugin_regressions` 回放 instructions 与别名，unit `skill_contributions_keep_ts_map_order` 回放 skills。目录环由独立 TS 子进程观测，3 秒超时后 SIGKILL 并由父进程清理临时目录；Rust 遍历测试通过可终止并回收的子进程防挂起；父进程的 20 秒期限不是性能通过上界。安全偏离的具体范围与候选登记文字见 RESULT，不把 Rust 防环结果写成 TS 期望。

仓库负控脚本现在也覆盖 `plugins.rs` / `contributions.rs` 的 publication 状态、refresh/dispose 等待、退休清理、共享 dispose、三种租约的 release 错误，以及 UTF-16、防环、合法别名、skills 插入序与同优先级覆盖。脚本支持 `lib:` / `regressions:` / `runtime:` 测试目标，仍要求实际断言失败、唯一变异锚点及 finally 字节恢复；编译失败与外部超时不计击杀。


修复轮 1 的独立金样 `traversal.json` 由 `generate-traversal.mts` 生成，原 `oracle.json` / `regressions.json` 字节保持不变。沿用上面的 Node / PDRS_BASELINE / LANG 设置，分别执行两次 `generate-traversal.mts <输出目录>`，比较两个 `traversal.json`。样本记录真实 TS 对 k=3/7/10/20 互链团的 1000ms 限时观察、目录/文件/链式越界链接、不可读条目、65 层目录、4097 条目、合法目录别名的完整贡献路径，以及含孤立代理的 MCP serverName。TS 的超时记录只声称在截止时仍未返回。

Rust 每次贡献目录遍历用全局 dev+ino 集合（Unix）只展开首次遇到的物理目录；非 Unix 使用规范化路径身份。无环别名也去重，保留首次访问的词法命名与路径。遍历使用显式 DFS 栈，没有条目预算、Markdown 候选预算或深度上限，也没有超限错误与 stderr 诊断。规范化条目必须位于规范化插件根内，否则跳过；两参数 `load_plugin_commands` 以 base 为根。插件三个贡献族和 style-only 传入实际插件根。原有 ROOT-001 金样投影仍只排除那两条已知越界贡献。

`plugin_traversal` 的四个互链团与 24 层 DAG 统一使用 20 秒父进程防挂起期限（spawn 返回后起算）；子进程退出优先于期限检查，Drop 在全部退出路径 kill + wait。TS 探针中的 1000 ms 只是截断 TS 未返回探针的观察预算，与 Rust 的通过条件无关。线性/有界的确定性判据来自图夹具：每个非根物理目录恰好一份 Markdown、根目录为空，故贡献数等于已完成的非根目录展开数；再断言贡献的父目录 dev+ino 集合大小等于唯一目录数（clique 为 k、DAG 为 25，另加空根 1 次）。不作墙钟性能断言。负控 `markdown-cycle` 只去掉全局目录去重，必须触发确定性断言或 k=10 的防挂起期限。`markdown-alias`、根边界与 serverName 负控保持。原 entry/depth-budget 测试改名为 `plugin_markdown_entries_match_ts` / `plugin_markdown_deep_tree_matches_ts`，现在分别回放真实 TS 的 4097 个贡献和 65 层完整贡献；原超限诊断测试及对应负控已删除。

修复轮 2 的 `budget.json` 保留原始字节，仍由 `generate-budget.mts` 生成，包含 4800 个非 Markdown 资产、12 层双别名 DAG 与简单兄弟互链。DAG 验证物理目录首次路径投影，24 层独立压力在子进程内限时；简单环比较唯一正文集合，TS 可以经 ELOOP 截止后成功返回重复贡献，不概括成所有环不返回。

修复轮 3 的 `generate-unbounded.mts` → `unbounded.json` 与 `dependency-tree.json.gz` 同样带固定 TS 源树/导入闭包守卫，必须双跑逐字节相同。生成器只读基线实际安装的 node_modules，`dependency-tree.py capture` 跟随别名展开为独立逻辑目录，记录所有可 stat 的文件路径与 Markdown 正文（9010 个 .md）。压缩快照用正文驻留去掉重复存储；非 Markdown 内容未被 loader 读取，回放为空文件。输入不是手写 README 分布，也不把别名去重混入预算回归。快照哈希记录在金样，Rust 重放前核验。

物化时每个逻辑目录对应插件根内 backing pool 中的一个物理目录，子目录以链接暴露，文件只写入 backing pool；不经任何 node_modules 路径写文件。真实树位于 `skills/s0/vendor/node_modules`，TS 实测结果完整回放；第二个输入含 5000 个 README.md、54000 个 .js、3 个 skill；第三个输入把 node_modules 放到 80 层无环路径后，完整比较 commands / skills / output-styles / style-only。运行方法同上，将生成器改成 `generate-unbounded.mts`，双跑比较两份输出中的两个文件。

新负控恢复 4096 候选预算，分别使真实依赖树和 README/JS 回放转红；恢复 64 层上限，使深 node_modules 的四入口回放转红。宽树和 65 层回放各有恢复对应预算的负控，非 Markdown 资产仍有恢复全条目预算的负控。DAG/简单环负控只去掉去重。所有负控要求专属断言文本与选中测试的 FAILED 行；编译失败、未执行测试及外部超时不计击杀。最终双跑哈希、断言失败摘要和检查日志见指定 RESULT 的「修复轮 3」。

修复轮 4 的 `generate-boundary.mts` → `boundary.json` 沿用固定 TS 源树、clean tree 与完整导入闭包守卫。按上方命令双跑并逐字节比较，所有写入均在新建的 `/private/tmp` scratch 目录内。四个回归完整比较 commands / skills / output-styles / style-only：配置 base 越界符号链接（按 ROOT-001 从 TS 投影剔除外部贡献）、插件根符号链接（保留词法路径）、`/private/tmp/...` 绝对配置路径（Node join 仍追加在插件内）、外部不可读 Markdown。最后一种真实 TS 因 readFile 的 EACCES 丢弃整个配置目录，而 ROOT-001 在读取前跳过越界文件、保留根内安全贡献；保留值由去掉外部链接后再次运行真实 TS 的 control 提供。两侧均独立验证 chmod 000 确实拒绝原生读取，不把 stat 成功说成 EACCES。对应四项负控为 `markdown-base-symlink` / `markdown-root-symlink` / `markdown-absolute-configured` / `markdown-outside-unreadable`，记录在 RESULT「修复轮 4」。

`markdown-expansion-identity` 保持贡献数量但把所有贡献路径改成第一个目录的路径，必须由子进程内的物理目录身份断言确定性转红（1 ≠ k），验证身份断言本身生效；不依赖防挂起期限。另复跑 `markdown-cycle` 和 `markdown-dag-no-dedup`，检查移除全局去重时仍会被期限/贡献断言抓住。

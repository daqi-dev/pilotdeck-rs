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

补充样本覆盖 2047/2048/2049 个 UTF-16 码元、CJK 跨字节边界、代理对在截断点两侧、孤立高低代理、ECMAScript trim、skills 的首次插入位置和各级优先覆盖，以及合法目录别名。`plugin_regressions` 回放 instructions 与别名，unit `skill_contributions_keep_ts_map_order` 回放 skills。目录环由独立 TS 子进程观测，3 秒超时后 SIGKILL 并由父进程清理临时目录；Rust 防环测试也通过子进程实现相同超时边界，避免 `spawn_blocking` 阻塞测试进程退出。安全偏离的具体范围与候选登记文字见 RESULT，不把 Rust 防环结果写成 TS 期望。

仓库负控脚本现在也覆盖 `plugins.rs` / `contributions.rs` 的 publication 状态、refresh/dispose 等待、退休清理、共享 dispose、三种租约的 release 错误，以及 UTF-16、防环、合法别名、skills 插入序与同优先级覆盖。脚本支持 `lib:` / `regressions:` / `runtime:` 测试目标，仍要求实际断言失败、唯一变异锚点及 finally 字节恢复；编译失败与外部超时不计击杀。

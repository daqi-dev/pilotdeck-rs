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

场景包括：空目录；合法插件；缺字段、错误类型及 JSON 语法错误；可选字段宽松投影；默认与配置目录；Markdown 前言和嵌套命名；输出样式及仅输出样式加载；inline/file hooks；MCP/LSP（含 150 层嵌套配置）；同名同来源与不同来源；内置技能；独立技能仅根 SKILL.md；内置插件磁盘重载/失败回退/禁用；磁盘插件不受 builtin 开关影响；不可读父目录/插件目录/Markdown 文件；目录与文件符号链接、断链；UTF-8 枚举顺序；失败 refresh 保留旧 generation 与 lease，删除则正常退休。

权限夹具在 Unix 非 root 用户执行。Rust 测试通过独立原生 `read_dir` / `read` 确认 `chmod 000` 的拒绝效果，失败时不跳过、不算通过；Drop 只恢复并清理该测试创建的临时目录。大小写仅有差别的文件名会受宿主文件系统影响（生成与回放须使用相同文件系统语义）。

实现负控（会临时改 Rust 源码；一次仅运行一份，确保工作区独占）：

```sh
python3 src/pilotdeck-gateway/tests/fixtures/plugin-loader/negative-controls.py \
  _tmp/20261022-a63-pr7b/negative-controls
```

每次检查唯一替换锚点，先执行 cargo-slots，再编译并运行对应金样测试。必须出现实际 `test result: FAILED`，编译失败不算命中；每项用 finally 恢复原始字节，汇总含恢复后的 SHA-256。参数可追加 mutation-id 仅重跑指定项。首次正控与每项失败摘要、范围外缺陷、最终检查结果均见工作区 `_tmp/20261022-a63-pr7b/RESULT.md`。

MCP/LSP 桥接沿用既有 `serde_json::Value` 表示，使用显式栈转换，遵守共用 `MAX_ARRAY_DEPTH=2000` 护栏；超界返回 `Maximum call stack size exceeded`，该区间不声称与 V8 无界 JSON.parse 等价。UTF-16 孤立代理项转为 U+FFFD，非有限数字按 JSON 投影为 null，这也是此既有表示的边界。TS 150 层探针以序列化字符串保存服务器对象，防止夹具读取器自身的 128 层限制干扰被测产品。

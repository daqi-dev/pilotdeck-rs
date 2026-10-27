# #879 pr3：进程 cwd 路径解析

固定 TS 基线 cc7c4e6、Node v22.22.0；8 格 × 10 个入口，每一入口在新子进程中调用。加载模块后先 chdir，再删除 cwd，之后才调用待测入口，避免 Node cwd 缓存假成功。清除 PILOTDECK_RUNTIME_ROOT，防止宿主相对 runtime root 改变 cwdReads。

真实入口：Node path.resolve、getGlobalMcpConfigFilePath、getProjectMcpConfigFilePath、getPilotProjectConfigFilePath、patchProjectScopedMcpSpec（FunASR / browser-use）与 createMcpToolDefinitionsFromRuntime 返回工具的 execute（读取 Markdown 图片）。最后一项只替换 MCP 网络端口，文件读取和 bridge 实现为原版 TS。

两相对参数时 cwd 删除会使 path.resolve 抛出 ENOENT / uv_cwd；绝对 path 或绝对 base 绕过 cwd。bridge 逐图捕获路径错误，保留文字。browser-use 还通过 Playwright 安装路径查询 process.cwd，绝对 root 也会失败；该事实不能扩推为通用 path.resolve 的行为。

absolute-base / absolute-path 只在 resolve 一列分别覆盖两个参数的短路；其余入口接收同一个绝对输入。funasr-home 单独覆盖「绝对 projectRoot + 相对 pilotHome」；load / load-project 实跑 loadMcpServerConfig 的全相对与仅 projectRoot 相对两条错误路径。registry provider、ConfigStore 的完整 TS 调用链不在该金样的实测声明内；Rust 测试仅核必要错误传递。

生成命令（输出目录可以存在，oracle.json 必须不存在）：

```sh
export PDRS_BASELINE=/Users/da/ws/pilotdeck-rs/resources/PilotDeck
node --disable-warning=ExperimentalWarning --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/metadata-process-cwd/generate.mts <输出目录>
```

重生成守卫：`metadata::process_cwd_tests::metadata_process_cwd_regenerates_identically`（ignored）。原始 stdout 由执行者保存，oracle 记录固定 commit 与原版文件 SHA-256。

负控：`negative_controls.py <全新输出目录> [变异名…]`，调用者必须设置 CARGO_TARGET_DIR。26 个变异全部记 KILLED / SURVIVED / INVALID，任一非 KILLED 非零；过期锚点、未提交源码、编译或环境失败都记 INVALID，绝不当 kill。只使用默认 HEAD 守卫。整套多轮重编可能超过 20 分钟，提交后由协调者在宿主分批执行；本树未提交，不绕过守卫。

fix-r2：pilot-project 八格直接消费 oracle；loader/store 以 Some(".") 选择 input 对应的真实项目。
project_config_path 保留 absent/empty project 跳过语义，其相对 cwd 路径失败会返回 uv_cwd。
browser-use 不再覆盖 absolute-deleted 金样；删除未批准差异 A6.5-MCP-BROWSER-CWD-001。

每个变异只改一个源码站点。`--check` 只检查锚点；`--compile-only <全新目录> [变异名…]`
逐个应用并运行 `cargo check --tests` 后恢复，汇总 COMPILED / INVALID；正式运行也会先编译再判断 KILLED。
两种模式都使用调用方的 CARGO_TARGET_DIR 和共享 HEAD 守卫。负控期间同树禁止其他 cargo。

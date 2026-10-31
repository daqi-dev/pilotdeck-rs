# #1137 原生注册的空 marker 归属

生成器直接调用固定 TS cc7c4e6 的 `createRegisteredWebProjectResolver` 与 `createDialogProjectRegistry`，Node 固定 v22.22.0。Unix 上共 15 格，Rust ownership 与 provenance 分别钉住固定数量，ownership 还显式拒绝空 cases。三种空 marker（空串、含 Unicode 的空白、仅换行）实测均未注册，dialog 返回 `PROJECT_NOT_FOUND`，文件字节不变；当前项目及带前后空白的当前项目被接受，其他项目被拒绝。

| 新边界 | TS / Rust 归属结果 | 保留内容 |
|---|---|---|
| `.`、`./`，进程 cwd 不同于 project | 拒绝 | 原始文件字节 |
| `.`、`./`，进程 cwd 等于 project | 接受 | 原始文件字节 |
| 当前项目尾 `/`、CRLF 结尾 | 接受 | 原始文件字节（不 trim 后回写） |
| marker 是目录 | 拒绝 | 目录类型和空目录内容 |
| marker 是悬空软链 | 拒绝 | 链接类型、`missing-target` 文本和目标仍不存在 |
| marker 原始字节 `ff` | 拒绝 | 原始字节 `ff` |

相对 marker 必须按**进程 cwd**解析。TS 探针在自己的临时目录中切换 cwd，并在 finally 恢复；Rust 原15格测试显式传入固定临时 cwd：project 与 other 均来自隔离目录，测试不读取或改动全局 cwd。生产入口复用 `resolve_process_path`，仅相对 marker 触发进程 cwd 读取。文件前后保留判据直接比较 Buffer / Vec 原始字节；oracle 的 `before` / `after` 额外记录路径占位后的 hex 以便跨目录复现，归一化不能代替原始字节判据。目录/软链没有文件正文，比较其节点状态。

目录、悬空链接和非法 UTF-8 格的 TS 入口会尝试 legacy ID 恢复；本 fixture 的含连字符临时路径无法按旧 slug 解回真实目录，本轮结果限定在这些实际采集的格，不宣称覆盖整个 legacy 恢复策略。非法 UTF-8 的错误形状有意不对齐：TS `readFile(..., 'utf8')` 以 U+FFFD 解码，再由 registry 返回 `PROJECT_NOT_FOUND`；Rust `read_to_string` 返回 `InvalidData`。实测本格两边都拒绝且不改字节；这里不声称所有非法 UTF-8 输入的行为等价。

Rust `register_project` 是 native 启动的 provisioning 入口，不等同于 TS 的只读 dialog 入口；本单仅对齐**已有 marker 能否证明当前项目归属**。空 marker 返回 native 启动错误并保留文件，不修补未知归属。目录读取报 IO 错误；悬空软链走既有发布逻辑，hard_link 因节点已存在失败，链接不被覆盖。这里不声称两个入口的错误形状相同，也不改变 TS UI 手动新增项目的行为。

TS coordinator 只提供对象引用，没有 Clone API。`observations.alias` 记录同一对象另一引用依次观察 reserve、同 key 冲突、release；Rust 的 `replacement_clone_shares_state` 覆盖同一组状态，专属变异把产品 `Clone` 改成独立空状态，绑定其首条 reservation 断言。操作全部同步，无等待、取消或前驱完成条件。

```sh
PDRS_BASELINE=/absolute/read-only/PilotDeck node --disable-warning=ExperimentalWarning \
  --import /absolute/read-only/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/native-cwd-marker/generate.mts /absolute/new-output
```

输出目录递归创建，oracle 以 `wx` 独占写入；临时项目只建在输出目录内。忽略测试 `native_process::tests::review::marker_regenerates_identically` 逐字重生成校验。负控沿用 `../native-process/negative_controls.py`，提交后使用调用方的 `CARGO_TARGET_DIR`，选取 `replacement-clone-independent-state blank-project-marker untrimmed-project-marker foreign-project-marker rewritten-blank-project-marker marker-oracle-baseline regenerated-marker-oracle`。每格先编译、正控/负控/恢复正控逐项记录；源码不等于 HEAD 则报 INVALID，不得绕过。

fix-r1 追加负控选取：`empty-marker-cases-ownership short-marker-cases-ownership empty-marker-cases-provenance relative-marker-project-base relative-marker-always-rejected trailing-slash-marker-rejected crlf-marker-untrimmed directory-marker-accepted dangling-marker-accepted invalid-utf8-marker-accepted invalid-utf8-marker-rewritten directory-marker-rewritten dangling-marker-rewritten marker-before-oracle marker-after-oracle literal-project-marker-rewritten marker-generator-cwd`。相对路径回退变异击中 cwd 不匹配格，全部拒绝相对路径的变异击中 cwd 匹配格；后三类节点分别配有保留判据变异。`literal-project-marker-rewritten` 专门防止路径占位归一化掩盖实际改写；`marker-generator-cwd` 改坏真实 TS 探针的 cwd 分组，由重生成守卫捕获。生成器以 realpath 统一 macOS `/var` 与 `/private/var` 的临时根拼写，避免把路径别名当成 cwd 不同。


## 路径边界（fix-r2）

`observations.paths` 使用 Node v22.22.0 实测 `path.resolve`、真实 project resolver / registry、`resolveProjectStorageId`。四个 project 输入（relative、尾 `/`、`..`、symlink）保存词法绝对身份；Rust 在解析阶段规范化，首次发布后重新解析参数、再次校验同一 marker。物理路径仅用于 home/project 隔离检查，软链不能绕过隔离。TS 的 marker 发布为探针种子，真实调用的是后续只读 resolver；不宣称 TS 存在相同 native provisioning API。

五个 request/submit projectKey 格在 cwd=other 下运行，`.`、`./`、空串拒绝，`../project` 与绝对 project 接受。Requests 与 Services 的 projectKey 守卫共用进程路径解析。仅 deleted-cwd / 请求边界 ignored worker 修改自己的子进程 cwd，父测试用共享 30s 有界装置驱动；负控绑定 worker 完整测试名及专属断言。cwd 删除后，绝对 marker 和绝对 project 解析都不读取 cwd。

`Services::Cwd::set` 属于不同契约：TS `ProjectRuntimeRegistry.setSessionCwd` 原样保存 `.` override，native 暂限定会话工作目录仍在已配置项目。放开此限制涉及会话 runtime/tool cwd、reload 及隔离策略，留待独立跟进，不把 marker 语义强加给它。两个 Metadata 消费者的 `cwd=project` 也留待独立跟进：真实 TS session-model 探针证明相对 marker 可能制造归属，但直接改成 `current_dir()?` 会破坏 cwd 删除时绝对路径的行为。需要共享 Metadata 的惰性 cwd/错误传播设计；本 fixture 不声称这两个消费者已对齐。ProjectResources、sidecar deployment、tools permission 的 cwd 也是会话执行目录，保持项目值。

新增负控名与完整绑定以 `../native-process/negative_controls.py` 为正本。修复前清洁 HEAD 单样本不替代修复后证明；修复后须由协调者提交，再运行全部 #1137 格（先逐格 compile，再正控/变异/恢复；任何非 KILLED 非零）。脚本无 quiet cargo，沿用 `mutating_sources`/`run_owned`，不能以未提交源码绕过 HEAD 守卫。

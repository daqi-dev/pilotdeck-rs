# #1137 原生注册的空 marker 归属

生成器直接调用固定 TS cc7c4e6 的 `createRegisteredWebProjectResolver` 与 `createDialogProjectRegistry`，Node 固定 v22.22.0。三种空 marker（空串、含 Unicode 的空白、仅换行）实测均未注册，dialog 返回 `PROJECT_NOT_FOUND`，文件字节不变；当前项目及带前后空白的当前项目被接受，其他项目被拒绝。

Rust `register_project` 是 native 启动的 provisioning 入口，不等同于 TS 的只读 dialog 入口；本单仅对齐**已有 marker 能否证明当前项目归属**。空 marker 现在返回 native 启动错误并保留文件，不修补未知归属。缺失 marker 的既有原子发布逻辑、其他项目错误及注册目录选择不变。这里不声称两个入口的错误形状相同，也不改变 TS UI 手动新增项目的行为。

TS coordinator 只提供对象引用，没有 Clone API。`observations.alias` 记录同一对象另一引用依次观察 reserve、同 key 冲突、release；Rust 的 `replacement_clone_shares_state` 覆盖同一组状态，专属变异把产品 `Clone` 改成独立空状态，绑定其首条 reservation 断言。操作全部同步，无等待、取消或前驱完成条件。

```sh
PDRS_BASELINE=/absolute/read-only/PilotDeck node --disable-warning=ExperimentalWarning \
  --import /absolute/read-only/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/native-cwd-marker/generate.mts /absolute/new-output
```

输出目录递归创建，oracle 以 `wx` 独占写入；临时项目只建在输出目录内。忽略测试 `native_process::tests::review::marker_regenerates_identically` 逐字重生成校验。负控沿用 `../native-process/negative_controls.py`，提交后使用调用方的 `CARGO_TARGET_DIR`，选取 `replacement-clone-independent-state blank-project-marker untrimmed-project-marker foreign-project-marker rewritten-blank-project-marker marker-oracle-baseline regenerated-marker-oracle`。每格先编译、正控/负控/恢复正控逐项记录；源码不等于 HEAD 则报 INVALID，不得绕过。

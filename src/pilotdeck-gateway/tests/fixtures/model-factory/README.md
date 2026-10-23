# 模型 factory 代际 TS 金样（#584 / A6.9-pr3b）

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器先以 `baseline_source_guard.mjs` 绑定整个 src Git blob，再记录入口的相对 import 闭包 SHA-256。Rust replay 通过 `common/baseline.rs` 核查来源。

真实调用链：`ProjectRuntimeRegistry.buildRuntime` → `ProjectRuntimeResourcesBundle.stage` → `ProjectModelRuntimeBundle` / `ProjectRouterRuntimeBundle` → `createRouterModelInvokerPort` → `createDurableModelInvokerPort` → registry / native provider → `streamModel`。其余资源族使用无副作用 fake；录制器是记录调用顺序的探针（Rust 对应真实 Recorder + TranscriptStore append 探针）。没有复制或改写 TS 实现，CJS 仅在内存转译以隔离未涉及的依赖。

网络边界：TS 注入 fetch Response/ReadableStream；Rust 注入 ModelHttpTransport/Body，两边喂同一 Anthropic SSE 字节。取消后检查 HTTP body 被取消 / 丢弃；只关闭 model handle 不能替代这个检查。此证据覆盖真实 HTTP 控制流，未声明覆盖 TCP、TLS、真实 provider 或代理实效。

金样包含：旧请求已到 HTTP 时 reload；旧代 metadata=1000、新代=2000；各项目代际独立 registry（provider generation 都从 1 起）；新轮次向新代 URL 发请求；取消在没有进一步 next() 的情况下抵达 HTTP；旧项目租约 drain 前不释放、drain 后释放；durable 请求先于 provider、event 先于 yield；同一 registry 替换 generation 1→2，替换前已接受但未 pull 的调用保留旧代、旧 registration.dispose 不移除新注册。

```sh
export PDRS_BASELINE=/absolute/PilotDeck
export PDRS_GIT=/usr/bin/git
out=$(mktemp -d /tmp/pdrs-model-factory.XXXXXX)
timeout 120 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/model-factory/generate.mts "$out"
# 用新的 out 再生成并 cmp，两次与入库 oracle.json 逐字节一致。
CARGO_TARGET_DIR="$PWD/src/target" timeout 600 cargo test --manifest-path src/Cargo.toml \
  --offline -p pilotdeck-gateway --test model_factory -- --test-threads=2
```

`Families::unwired().with_native_models()` 安装真实 model/router 资源族，其余族仍由各自总装单提供。每轮从 RuntimeLease 的 ProjectResources 取 RouterResources，再调用 ports。调用方持有 runtime lease 到 turn settle；provider registry 的调用租约另外保护在途 stream/complete，drop 也释放。provider 尾项归 #572，插件 customRouter 注入归 #588。

现有 HTTP 层无 non-streaming `complete()`，原生 provider 明确不广告该能力；启用 token-saver 的 router 构造会报告 judge_invoker_missing。注入支持 complete 的 provider 可正常使用注册表的 complete 租约。此存量缺口列入 RESULT「范围外」，本单不把流式聚合伪装成 TS 非流式请求。

补充金样：9 项 registry API 结果（空/空白/BOM provider id、重复注册、重复 lease.release、unregister、missing/closed 错误）；替换保留 JS Map 原插入位置的多错误清理及聚合；complete 调用在取消 settle 前保持 lease，whenDrained 的前后状态；原生 provider 默认构造分支（TS 只在调用层注入 fetch，router 使用保留的 model-runtime 兼容注入口）；4 组真实 `ensureRouterConfig` 默认与显式覆盖输入。源闭包共 728 个文件。

负控脚本只修改本树，SHA-256 校验恢复，复用唯一 CARGO_TARGET_DIR；逐个 cargo 先查 slots、每个最多 100 秒，外围仍须 timeout 600。只接受已编译测试的断言失败，不把编译错误当转红。

```sh
CARGO_TARGET_DIR="$PWD/src/target" CARGO_BUILD_JOBS=4 timeout 600 python3 \
  src/pilotdeck-gateway/tests/fixtures/model-factory/negative-controls.py /tmp/pdrs-584-negative-new
# 可在输出目录后跟一个或多个 control 名称，只重跑选定负控。
```

10 个变异：新调用误拿旧代、drain 前释放、HTTP 不接取消、旧 registration 误删新代、替换改变 Map 清理次序、complete 提前释放 lease、factory 跳过 recorder、漏校验 JS BOM 空白、原生族不注册 provider、忽略项目 router 默认值。各项的测试名与精确变异锚点直接见脚本。

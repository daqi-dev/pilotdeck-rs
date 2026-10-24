# 模型 factory 代际 TS 金样（#584 / A6.9-pr3b）

固定基线 `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。生成器先以 `baseline_source_guard.mjs` 绑定整个 src Git blob，再记录入口的相对 import 闭包 SHA-256。Rust replay 通过 `common/baseline.rs` 核查来源。

真实调用链：`ProjectRuntimeRegistry.buildRuntime` → `ProjectRuntimeResourcesBundle.stage` → `ProjectModelRuntimeBundle` / `ProjectRouterRuntimeBundle` → `createRouterModelInvokerPort` → `createDurableModelInvokerPort` → registry / native provider → `streamModel`。其余资源族使用无副作用 fake；录制器是记录调用顺序的探针（Rust 对应真实 Recorder + TranscriptStore append 探针）。没有复制或改写 TS 实现，CJS 仅在内存转译以隔离未涉及的依赖。

网络边界：TS 注入 fetch Response/ReadableStream；Rust 注入 ModelHttpTransport/Body，两边喂同一 Anthropic SSE 字节。取消后检查 HTTP body 被取消 / 丢弃；只关闭 model handle 不能替代这个检查。此证据覆盖真实 HTTP 控制流，未声明覆盖 TCP、TLS、真实 provider 或代理实效。

金样包含：旧请求已到 HTTP 时 reload；旧代 metadata=1000、新代=2000；各项目代际独立 registry（provider generation 都从 1 起）；新轮次向新代 URL 发请求；取消在没有进一步 next() 的情况下抵达 HTTP；旧项目租约 drain 前不释放、drain 后释放；durable 请求先于 provider、event 先于 yield；同一 registry 替换 generation 1→2，替换前已接受但未 pull 的调用保留旧代、旧 registration.dispose 不移除新注册。

```sh
export PDRS_BASELINE=/absolute/PilotDeck
export PDRS_GIT=/usr/bin/git
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/src/target}"
out=$(mktemp -d /tmp/pdrs-model-factory.XXXXXX)
timeout 120 node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/model-factory/generate.mts "$out"
# 用新的 out 再生成并 cmp，两次与入库 oracle.json 逐字节一致。
timeout 600 cargo test --manifest-path src/Cargo.toml \
  --offline -p pilotdeck-gateway --test model_factory -- --test-threads=2
```

`Families::unwired().with_native_models()` 安装真实 model/router 资源族，其余族仍由各自总装单提供。每轮从 RuntimeLease 的 ProjectResources 取 RouterResources，再调用 ports。调用方持有 runtime lease 到 turn settle；provider registry 的调用租约另外保护在途 stream/complete，drop 也释放。provider 尾项归 #572，插件 customRouter 注入归 #588。

registry adapter 与 TS 一样始终暴露 `complete()` 分派端口，混合注册表、全 native 和空表均可构造启用 token-saver 的 router；调用时按 request.provider 取租约并分派。混合表的 judge 可正常调用 complete-capable provider；空表到调用时才报 provider_not_found。直接注入不提供 complete 的非 registry invocation port 仍在 router 构造时报 judge_invoker_missing。

`supports_complete()` 只表示接线存在（TS dispatch-exposes 语义），返回 true 不保证 `complete()` 成功，也不保证请求所指 provider 已注册或有 complete 实现。调用方即使检查为 true 也必须处理调用错误；若需要错误回退，应依据实际调用结果。#604 后 native provider 通过 http::complete_model 实现 complete()，返回 true；registry 返回 true 是其分派端口的契约。

`empty-missing-provider` 金样刻意越过 TS 类型边界，删除 request 的 provider 属性后调用真实 registry adapter：端口仍存在、router staging 成功，调用返回 `provider_not_found` / `Provider undefined does not exist.`。Rust 新测试单独回放此项，覆盖 `missing("undefined")` 分支；既有 `empty`（provider=b）金样保留。

#604 实现 HTTP complete()，native provider 返回 true。全 native 金样只验证 enabled router staging 与 supports_complete；混合金样调用注入的 judge-b complete（不是 native HTTP complete），空表金样按设计返回 provider_not_found。native HTTP complete() 的完整 TS oracle 验证在 `tests/model_complete.rs`（`tests/fixtures/model-complete/`），不在本 fixture。

补充金样：9 项 registry API 结果（空/空白/BOM provider id、重复注册、重复 lease.release、unregister、missing/closed 错误）；替换保留 JS Map 原插入位置的多错误清理及聚合；complete 调用在取消 settle 前保持 lease，whenDrained 的前后状态；原生 provider 默认构造分支（TS 只在调用层注入 fetch，router 使用保留的 model-runtime 兼容注入口）；4 组真实 `ensureRouterConfig` 默认与显式覆盖输入。源闭包共 728 个文件。新增 registryRouterCases 来自真实 ProjectModelRuntimeBundle / ProjectRouterRuntimeBundle：混合（native a + complete b，judge=b）、全 native（judge=a）、空表（合法配置声明 b，但注入返回空注册表，judge=b）；后两种中的全 native 只对照 staging；混合对照 registry port 对注入 judge-b 的 complete 调用，空表对照 provider_not_found；均不经过 native HTTP complete（后者见 `tests/model_complete.rs`）。每次使用真实配置加载器校验配置。

上述命令的 PDRS_BASELINE 必须保留到 Rust replay；仅在设置该环境变量时，common/baseline.rs 才逐项比对真实源文件与 Git blob，默认未设置时只校验 fixture 的 commit 和哈希格式。

负控脚本只修改本树，SHA-256 校验恢复，复用唯一 CARGO_TARGET_DIR；逐个 cargo 先查 slots、每个最多 100 秒，外围仍须 timeout 600。只接受已编译测试的断言失败，不把编译错误当转红。

```sh
CARGO_BUILD_JOBS=4 timeout 600 python3 \
  src/pilotdeck-gateway/tests/fixtures/model-factory/negative-controls.py /tmp/pdrs-584-negative-new
# 可在输出目录后跟一个或多个 control 名称，只重跑选定负控。
```

15 个变异：新调用误拿旧代、drain 前释放、HTTP 不接取消、旧 registration 误删新代、替换改变 Map 清理次序、complete 提前释放 lease、factory 跳过 recorder、漏校验 JS BOM 空白、原生族不注册 provider、忽略项目 router 默认值。各项的测试名与精确变异锚点直接见脚本。

本轮增加 5 项：混合表错误要求全 provider complete、全 native staging 错误拒绝、空表 staging 错误拒绝、complete 忽略请求 provider、model staging 在无 Tokio 线程 panic。构建复用启动器的 CARGO_TARGET_DIR（本树 src/target）；缺失时只补此同一路径，不另建 target。

修复轮 2 再增 2 项（合计 17 项）：native provider 错报支持 complete，以及无 provider 的 complete 错报 `Provider null`。分别要求原生能力契约测试与新增无 provider 金样测试断言失败。registry 的混合 / 全 native / 空表暴露语义继续由既有三项负控守护。

修复轮 3 明确 `unregister(id)` 按 id 移除当前代，即使它已由 `replace()` 发布；`registration.dispose()` 才有所属代的身份保护。`unregisterAfterReplace` 金样覆盖旧 registration 保留新代、按 id 移除新代、等待其调用租约 drain，以及等待期间另注册的后继代仍可使用。

`providerValueCases` 越过 TS 类型边界，逐项回放 missing、显式 undefined、null、整数、小数、true/false、负零及大小数指数格式，分别核对 stream/complete 错误。错误文本使用共享 JS `String()` 转换；路由查找保持 Map 的类型语义，不把非字符串转换成字符串路由。每项都预先注册对应的字符串键，并通过真实 complete 正控证明该键可用。JSON 无法保留的显式 undefined 和负零由 Rust 按用例名还原。

本轮新增 7 个负控（合计 24 项）：unregister 错误保留 replacement、stream/complete 各自把非字符串错归 undefined、null/number/boolean 转换分别退回 undefined、非字符串错误命中同名字符串路由。原有两项 complete 负控随共享请求取租约函数更新锚点，仍守护原行为。

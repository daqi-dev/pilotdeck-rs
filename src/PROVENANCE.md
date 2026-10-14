# Rust sidecar 来源与适配记录

## A4 配置加载与 model_catalog_list（2026-10-14）

第 2 轮 CJK 对拍另见 `pilotdeck-gateway/tests/fixtures/collation/README.md`。
Node 22.22.0 / ICU 78.2 / CLDR 48.0 与 ICU4X collator/data 2.1.1
（CLDR 48.0.0 / ICU release-78.1rc）的 root Han 数据变体不同。
保留全新隔离进程的真实 WS response 与完整码位区段统计；没有下载替换
排序数据、增加 crate 或将候选登记为 approved。

`pilotdeck-gateway/src/config/` 与 `model_catalog.rs` 按只读 PilotDeck 基线
`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 独立实现。行为依据为
`src/pilot/config/loadPilotConfig.ts`、同目录各配置节解析器、
`src/model/config/`、`src/gateway/dialog/modelCatalog.ts` 与
`src/gateway/server/GatewayWsConnection.ts`；没有搬入这些 TypeScript 源码。
YAML 使用本仓 `pilotdeck-yaml`，JS 字符串与 wire 使用 `pilotdeck-protocol`。

`config/catalog-data.json`、`contracts.json`、`timezones.json` 由
`tests/record_config_data.mts` 在隔离 HOME/XDG/TMPDIR 的离线 Node 22.22.0 /
ICU 78.2 中导出基线运行时数据。目录 fixture 与诊断原件通过真实基线调用
录制，具体来源、SHA-256、边界及复现命令见
`pilotdeck-gateway/tests/fixtures/config/README.md`。没有复制源码、启动监听服务、
读取用户 PilotHome 或下载预编译程序。

来源仓（只读）：`/Users/da/ws/PilotDeck-rust-program-20261009/build/source-repository`。
来源 commit：`0eba113`。取文件仅使用 `git -C <来源仓> archive 0eba113 ...` 和 `git -C <来源仓> show 0eba113:<路径>`，未在旧现场写入或 checkout。

搬入范围：`rust/crates/{pilotdeck-agent,pilotdeck-transport,pilotdeck-protocol,pilotdeck-tokenizer}` → `src/<crate>`；`rust/vendor/tiktoken-rs` → `src/vendor/tiktoken-rs`。vendor 的 `LICENSE` 原样保留。所有搬入文件在原样阶段均逐字节核对源 commit。

## 适配例外

- `src/Cargo.toml`：合并旧 workspace 的 resolver、edition、rust-version 1.93、version、license-file 与 lint 表；成员只登记本次四个 crate 和保留的 CLI；exclude vendor；保留本仓 `publish = false` 和 CLI 继承的 AGPL 标识。只搬入四个 crate 用到的 workspace 依赖，版本保持旧钉死值；`pilotdeck-protocol` 的 path 改为本仓布局。crate 中直接声明的 uuid、chrono、base64、rustc-hash 等依赖维持原值。
- `src/pilotdeck-tokenizer/Cargo.toml`：vendor 路径从 `../../vendor/tiktoken-rs` 改为 `../vendor/tiktoken-rs`，适配从 `rust/crates/` 到 `src/` 的布局。
- `src/pilotdeck-protocol/src/bin/pilotdeck-protocol-probe.rs`：原样快照后移除；该 probe bin 是本任务明确排除的范围，源文件哈希仍保留以审计完整原样搬入。
- `src/pilotdeck-core/Cargo.toml`、`src/pilotdeck-core/src/lib.rs`：删除无实际能力的占位 crate。
- `src/pilotdeck-cli/Cargo.toml`：移除占位 core 依赖；补齐 workspace rust-version 与 lints 继承，使 CLI 同样受 `unsafe_code = "forbid"` 约束。
- `src/pilotdeck-cli/src/main.rs`：直接用 `env!("CARGO_PKG_VERSION")` 打印版本，保持现有输出。

- `src/pilotdeck-transport/src/bin/pilotdeck-agent-loop-sidecar.rs`：增加参数判定辅助函数，额外接受恰好一个非空且不以 `--` 开头的位置参数并忽略它，使 TS gateway 的 `args: [sidecarPath]` 能拉起 Rust。保留无参数、`--stdio`、`--version` 与非法参数退出码 2；增加参数矩阵回归测试、空串进程退出码测试和 usage 提示。

- `src/pilotdeck-tokenizer/src/implementation.rs`：`include_str!` 的词表资源路径从 `../../../vendor/tiktoken-rs` 改为 `../../vendor/tiktoken-rs`；布局减少一层，嵌入的词表字节保持原样。
- `src/pilotdeck-transport/build.rs`：构建时读取当前 git HEAD，注入 `PILOTDECK_SOURCE_COMMIT`，供 sidecar `--version` 输出；读取失败才使用 `unregistered-build`。跟踪 worktree HEAD、分支引用与 packed refs，提交或切换后重新构建会更新来源。
- `src/Cargo.lock`：按合并后的 workspace 重新解析锁文件，移除 core、登记导入依赖，直接依赖版本仍照旧钉死。

## 后续精简候选

以下仅登记，留到 S7 评估；本轮不修改 tokenizer、vendor 代码或资源。

- **o200k 词表重复嵌入**：`pilotdeck-tokenizer/src/implementation.rs` 的 `RANKS` 与 `vendor/tiktoken-rs/src/tiktoken_ext/openai_public.rs` 的 `o200k_base()`、`o200k_harmony()` 都嵌入约 3.6 MB 的同一词表。后续核实最终二进制是否重复携带，并评估复用 vendor ranks；直接改用 `o200k_base()` 前须验证自定义 Source 正则、普通文本与特殊 token 契约保持一致。
- **vendor 资源裁剪**：`vendor/tiktoken-rs/assets/` 中的 `cl100k_base.tiktoken`、`p50k_base.tiktoken`、`r50k_base.tiktoken`、`encoder.json`、`vocab.bpe`（约 5 MB）是当前 sidecar 未用资源候选。S7 核查 vendor API 与编译产物后再裁剪，保留来源哈希和许可证追溯。
- **依赖声明统一**：`pilotdeck-transport/Cargo.toml` 对 `pilotdeck-agent` 用裸 path、对 `pilotdeck-protocol` 用 workspace 表；S7 评估统一 workspace 依赖声明。

## 原始文件 sha256

以下完整哈希由原样搬入阶段的 `shasum -a 256 <文件>` 生成；路径相对 `src/`。这是来源哈希，适配后的文件以上方例外清单和本仓提交链追溯。

```text
f18d61a0dfcf0d2acb070bc3a60dd0d1c9fe1576c94f4cd5f4b10bcfb45b44cb  pilotdeck-agent/Cargo.toml
bb24871ea06d25845bfe108561506ea4e71b92c5e583ceed2caf1356c916f9d5  pilotdeck-agent/src/budget_terminal.rs
d6e1aa263533e8b9e24b3d4ddb83ce91532257f95030124910cc927ef33a7ac7  pilotdeck-agent/src/budget_terminal_policy.rs
4bc41c5424c5933e8d61e484053a5a3a50fc6ff557c5577f1093285541b29325  pilotdeck-agent/src/compaction_persistence.rs
a0163c532c8d0d51bcd810119a7c5a067fb6454acfba473491dbfd4b7ac6d426  pilotdeck-agent/src/compaction_persistence_policy.rs
6dcb79a3f1ae3e462dfe7de958b635e7ca66356124106b527377f05b977f9105  pilotdeck-agent/src/compaction_terminal.rs
7bbac0ae889517bff67c4946267aad6bba249620f702825d8ec091e8f8b8c06d  pilotdeck-agent/src/compaction_terminal_policy.rs
eb6e0decdaadc22b09db7a9f791f3fa721561d0d907783f40ec3d90d3c086b37  pilotdeck-agent/src/driver.rs
e1c85cc4a537f062c882201b8e2618fe99e1393a22ecafc1d71244f4de73f9e0  pilotdeck-agent/src/early_stream_recovery.rs
580007163ef28dcaf143cbc7322edd05a0c1e23c27b6ea6f13a49177a283b55f  pilotdeck-agent/src/early_stream_recovery_policy.rs
0dc97c849784f796b40dc57448ae150b14925421cc44f8df827104996c1092ae  pilotdeck-agent/src/large_file_repair.rs
1eae3795d5a796f364283ad160eef5914567c190cc398f6dc45760c0ec2f7d58  pilotdeck-agent/src/lib.rs
609b4b48799528fee1911a45b018bfd469bf9508da11c9381dde29d1f1feef87  pilotdeck-agent/src/model_event_reducer.rs
4faa792896846289fa790e5009bd35f28126c638ae735a15e018aca2acf67149  pilotdeck-agent/src/ordinary_error_recovery.rs
b45b334e71c653f62e5c57578ff84d3c89730316621db386d0c66c691c5aee78  pilotdeck-agent/src/ordinary_error_recovery_policy.rs
08e6ddda1fb2514a288b0f97f9a469be0a6c24226fdb43c1a88ba6fc38c981ba  pilotdeck-agent/src/output_recovery.rs
48a18329c08bbe5a80fe4ec14d83ab8da5ef5e83807f54d1f7a37f6461ce40de  pilotdeck-agent/src/output_recovery_policy.rs
023028980430e83ec96ff05395c97737e53abf2d13b232ba96d904a03798390b  pilotdeck-agent/src/ports.rs
1b21a2ee424507616a1264f0b815ab81d31e0a8879f10af1c71e62f06e2e14eb  pilotdeck-agent/src/request.rs
a59928120a3d073ef3b4f56459d96bebaf225f57ce41955952a5029850314402  pilotdeck-agent/src/request_assembly.rs
480a24968665765d54d2498563b51dadcffa1fdde4e2eff953df59f41aa57923  pilotdeck-agent/src/request_materialization.rs
1ed970ee8d3df3193e53a81c92ba2751a394eae93b635cb00d16e6d2a61fa9b7  pilotdeck-agent/src/request_materialization_policy.rs
7b61da04a9d25eb4abb7734654dbbcc144ba045f47ea5f483b169ce08dd093dc  pilotdeck-agent/src/session_state.rs
3b6d4d2e48efe7701650761976a549bfee245f9d66f4a2ea8f5ff9e6563b9dd6  pilotdeck-agent/src/session_state_projection.rs
bacbf39ab676b5cd4d18ef657d58899ce4ce33525dd7992257e01a5780fb51fa  pilotdeck-agent/src/source_invocation.rs
3319f3ff5f0190af9686a5449b19786bea61a094b588c32d8b1cdbc48817a2c2  pilotdeck-agent/src/source_model.rs
1d0d2d3a3f2d307eb4be619536fe039f3031f849b1337cd498336afdce468569  pilotdeck-agent/src/source_model_reducer.rs
56602bedb48a4786a36edf0bff31600b8f1411714a3cd3201bc7480ddd20c5fd  pilotdeck-agent/src/tool_context.rs
fc67791bbcb5cece89fe9ee5887acd6c47a7c804a6e3244b515f75ac98a6f104  pilotdeck-agent/src/tool_context_policy.rs
4c0776fbbe1bd60161515ef643b48c95e958d8ae24ad068970685fa26705b96a  pilotdeck-agent/src/tool_error_recovery.rs
e5857a00e8e9d8ac7d7a7306e6e7395e957890beb32008ca1dcd06f741e9c00e  pilotdeck-agent/src/tool_error_recovery_policy.rs
b27056fc0f551fa2ba31ee674952a8bf6b4d82745f6af4d2a3589e03c4d1a39f  pilotdeck-agent/src/tool_failures.rs
75215d49a543df5be0170201f1bb82eaeb00a5f80059880ed514833be469e482  pilotdeck-agent/src/tool_failures_policy.rs
d3459cef0d72bc3324380cfe7f9ad6e0f9651df16acd7eed706604c80704e828  pilotdeck-agent/src/tool_projection.rs
1afb70361e4f726827c2b6fc3d32e7beb752d77f7b667f3eb5d930e640b4ca7b  pilotdeck-agent/src/turn.rs
6215f0a302b8367df8ec0f64ac0fb4cd64e54eb5b2b197161f16bc2cae9be6d4  pilotdeck-agent/src/turn_timeline.rs
1aabc6157fdc34bc3dbf5bdb196bd1b2f26492de5b65c470956cd1a09bf05909  pilotdeck-agent/src/turn_wire.rs
b1b40b3fe6a26b01641538b0c7d8b24b3a8dc976deb7a6a4ce3a87aa101318b6  pilotdeck-agent/src/usage_fallback.rs
160088ed64028910ddf8a5090e76417899785644ff8d1f2d3e0f3e21baf9e210  pilotdeck-agent/src/usage_fallback_policy.rs
50d27359d8925641f13bd43965641ee0d55fc264e5ead36ce84d2ff3aa85203a  pilotdeck-agent/tests/budget_terminal.rs
d656d7fc183d70f3ae9b5b845459334bf8fc490d478a200760f96cd62260b5b1  pilotdeck-agent/tests/cancellation.rs
ac424808c50032ec359b039514a5f4c9b90e5c8a62a8e5b1a829021c590351c8  pilotdeck-agent/tests/compaction_persistence.rs
32dfc9acbcce9719060b6eec4f5dd9c721152fe032a3c4775657a77c10cf7d1a  pilotdeck-agent/tests/compaction_terminal.rs
477f2c69bf75590d36025f521b84517256a316dc0e76702a0a6697b302c72805  pilotdeck-agent/tests/driver_policy.rs
57ba47542a1fd829a210e773288f18b33ecdca0e509fc17fcd0075a237f4848f  pilotdeck-agent/tests/early_stream_recovery.rs
e47a18bb911a58c0b2350bca8cc0071a5bbccc5db51b05d253a7414e5bcacc89  pilotdeck-agent/tests/large_file_repair.rs
d7cfa11772aaea63aff7d083285cdfc3c2657e891bf11c75403db270ebceb166  pilotdeck-agent/tests/ordinary_error_recovery.rs
13b9e3848c4740f783d6ba05a2a999171efd6f5a88f633c976a0eb30163b3771  pilotdeck-agent/tests/output_recovery.rs
3772c377a6ef8c3f792c768f9b9ebc409143d64d65a01063dd50ea275d4d1ba4  pilotdeck-agent/tests/output_recovery_extension.rs
049b24400277d311d6291e30a3bbba5734a78c28b7b55ecfeb7af99c0656587f  pilotdeck-agent/tests/pure_helpers.rs
aee4fe6cc260d6ed641d34d8b9ea3394e88b995046f695a5d833cb00eaa76f04  pilotdeck-agent/tests/request_assembly.rs
2a539327a29e43d3035bddf24f41d31ac2c91dd5ee332645e1883f1ee72811a7  pilotdeck-agent/tests/request_materialization.rs
c78b63af62f9fba2e40505b3c166dd384cf55361ade91b4dbe678b66557bd0bf  pilotdeck-agent/tests/session_state_projection.rs
4c62f7e516d9728007d9a00982dcdc2c29be6dee9552ec034e32b0ea7b6c3bcb  pilotdeck-agent/tests/source_model_reducer.rs
896361afef82b3b81b45d29c903ffef160a74df043c370263ce6e38a38db78b3  pilotdeck-agent/tests/timeline.rs
764a5b2038f3b55310ddc795b7cd564aa3b5acdf9562ff631df7b827130d2b8a  pilotdeck-agent/tests/tool_context.rs
aa462ba11f979f65c6ae171669855d227ad47ada65a8a8f89bbb3682be03d32c  pilotdeck-agent/tests/tool_error_recovery.rs
5813e7991cdedffe79aa27870694b947c3665405a2dbb8247c888bea8b7707fa  pilotdeck-agent/tests/tool_failure_recovery.rs
784e2a45768f17637b73dddc16cb5679fefd678ae883a7f6dcf53aa851265bf6  pilotdeck-agent/tests/usage_fallback.rs
6220bbf168ea59c320ab8327e18538a745ef2ee58e0434f75b40f21208823f06  pilotdeck-transport/Cargo.toml
2e301c935b8ba1c4e36ef34a45f4c5fcec668e2b92866b2d29c60731334fdf68  pilotdeck-transport/src/bin/pilotdeck-agent-loop-sidecar.rs
e9b4792c46542f747275dd909b9c95439e863fd4ba69ab2367c173d7e9182010  pilotdeck-transport/src/default_factory.rs
7e515f4b69c92163e76bb904a18b9d7d6e901f9f85b36e1d2b337af06ff03b97  pilotdeck-transport/src/execute.rs
e00190039284f974705a37528ca6a1080c5ea6e66338fe88b1e834e38bf384c0  pilotdeck-transport/src/factory_payload.rs
9927d349d025c2ab7acbadabda359055ae87bcb9e39a77558f6ef32f8a294a21  pilotdeck-transport/src/host_operations.rs
92ebaafbf5978b81ce278a8db8a3f89e7187443f12800d54f4256f0784a5b6df  pilotdeck-transport/src/lib.rs
cda4ed51e8fea6ad66e1df8cf153f6505267854480aebef32f82d17e4992862a  pilotdeck-transport/src/model_admission.rs
1979ffe59b2fea57a76f2d6d779bc0bcfccbbb9e6eb8cb92da569e2d9d4e2eec  pilotdeck-transport/src/notifications.rs
95ce6162ce9223f9e45aff694ea16c78234cf4c60ccc45c973b3eb473fea7ef6  pilotdeck-transport/src/server.rs
615a0f13fedcab00b234866566cfe8dcbc02a61a46c5ecdffdca316023f49b6e  pilotdeck-transport/src/stdio.rs
cd48345ed072d0d90f76a1f2917ac117403f4a42994399fb9916deb7bb151dbf  pilotdeck-transport/src/system_clock.rs
9f97edeb6d8a4014a6c46bc299f388bfa45775c11fc5563940cb4a9b72ec997b  pilotdeck-transport/tests/bounded_capture.rs
b464b86eeb9350990a75cc10c9a5ac683b00c66917a2a8eca3cc06f0df1fd037  pilotdeck-transport/tests/bounded_capture_memory.mts
4dfd4b31d1ee5f9117790f38e7ef1db66c85a13d3daa4acc89f04db1323ccf65  pilotdeck-transport/tests/context_boundary.rs
f9a86da176f57e7dfb0eec2e40f888bedebf063f2b33a5e08ee70ef723e8c6ac  pilotdeck-transport/tests/error_boundaries.rs
e2ec404cf7ebda67713601220523746c7fa51ec2177dda4f767d1f4f6c42c499  pilotdeck-transport/tests/factory_payload.rs
ac77c66b531061d8ec6f0634796c2b6a41740e173b94790f612ab53243164ab5  pilotdeck-transport/tests/host_contract.rs
33e0643bae3de0d5de6f2a91d60e86e599bdf95621d3ecb46bb5d3034e5f5c37  pilotdeck-transport/tests/server_owner.rs
016d3d22dc13c65046e8a2bf1b1c42f7ae9ff2ef6d1aa771d0092c7462e03252  pilotdeck-transport/tests/system_clock.rs
8c0a8d276c45e7dae564756cec73637f2d16b6c7eaae93d022e8b31f12815c0a  pilotdeck-protocol/Cargo.toml
4e25e74fda555681d1f491862859dd299fbbc9bd9f720462751dca7fea8254b4  pilotdeck-protocol/src/admission.rs
cd4327c5013414973e7aa427e143bcc586739158fb9d2a6204c436d024aa5fa0  pilotdeck-protocol/src/bin/pilotdeck-protocol-probe.rs
59dbd9e3750b7076a13fefa426bd7b35c49f7b6e2bba64ac8988b233e3a67030  pilotdeck-protocol/src/codec.rs
d06334f455ed8bae0bfd143943ba5b01316c707c46a1b8465a6f97d1612148fc  pilotdeck-protocol/src/consumer.rs
3e64b256a7edf0388b9e8d44cf73cff1920361928e3dec36cb8caed0e4307c3a  pilotdeck-protocol/src/lib.rs
82c1e0fec034649b462cdac6284404ab241dd5483d4cee9fd4b105f1ffe6bdb0  pilotdeck-protocol/src/model.rs
c5a801864a4367030f9b5e4026aef1f380bf0c542605ea4a287313b8bfd714b4  pilotdeck-protocol/src/module.rs
c15ad8bea71446ed9c698217a6a3b59f274775ff725205deab87b235b5c6e095  pilotdeck-protocol/src/snapshots.rs
6f73e49335ef8aa09dd84b22af70b7dc948800a53e46a83b450bb47aad2e4085  pilotdeck-protocol/src/wire.rs
31d16283edc0732bb186ac2d13050d31ad54a06fd877ccbda0cf8f90327e0d73  pilotdeck-protocol/tests/codec.rs
b976f8aea3ca54b85a46b05178add18f672eadbe4207e9f606227eb713301a54  pilotdeck-protocol/tests/model_admission.rs
25bc1b5387ac0c11fc4166a437870d51ba7c25d834cf4779c1b8292c4ad17eaf  pilotdeck-tokenizer/Cargo.toml
a3146a6a7190a97b5e06d645731792525631821e3b0fc028adaf9866bd68c09a  pilotdeck-tokenizer/src/implementation.rs
a1372346ca05133d9ea06165f8537d965b599799177192f3fcac60cd38c46169  pilotdeck-tokenizer/src/lib.rs
bdd52163593625027e456d59bd9bb7f9344099cfe5b4d53e051645433a38f314  pilotdeck-tokenizer/tests/ordinary.rs
20425523e4c9a8b0f6ea48e674443316377b975aa9d4635b5d06612e639fc976  vendor/tiktoken-rs/.cargo_vcs_info.json
395e230607f5be26773987160b4234c0bdcf21bfb009934a2f8a428215617d82  vendor/tiktoken-rs/Cargo.lock
2966c4c103146f70ad0343a3012543d23451965c279f81b09d3a8c24c306d333  vendor/tiktoken-rs/Cargo.toml
e9e9b1770a8c5c20b4a468c3c0373dd3b9917458182b0125bd6b142726e78e66  vendor/tiktoken-rs/Cargo.toml.orig
f7c6ddf9d84fd7b8ad5917e4074d4c05e4c1dfb752a28a0058f06bd0f5e2edcc  vendor/tiktoken-rs/LICENSE
6666bac4fe1c437774260ddfece43b192ecc826addd297959224ecd6919cc30c  vendor/tiktoken-rs/PROVENANCE.md
223d6c2ed56752d51dd1df60e9116e9e7220e848d2d9ca10cc6d9096c34352ff  vendor/tiktoken-rs/README.md
223921b76ee99bde995b7ff738513eef100fb51d18c93597a113bcffe865b2a7  vendor/tiktoken-rs/assets/cl100k_base.tiktoken
6401aa8aac4e480b02ed2713037078c26fab6fc9f1882012e746fe9bd87bc99b  vendor/tiktoken-rs/assets/encoder.json
446a9538cb6c348e3516120d7c08b09f57c36495e2acfffe59a5bf8b0cfb1a2d  vendor/tiktoken-rs/assets/o200k_base.tiktoken
94b5ca7dff4d00767bc256fdd1b27e5b17361d7b8a5f968547f9f23eb70d2069  vendor/tiktoken-rs/assets/p50k_base.tiktoken
306cd27f03c1a714eca7108e03d66b7dc042abe8c258b44c199a7ed9838dd930  vendor/tiktoken-rs/assets/r50k_base.tiktoken
1ce1664773c50f3e0cc8842619a93edc4624525b728b188a9e0be33b7726adc5  vendor/tiktoken-rs/assets/vocab.bpe
cc7075e932c91944ce41fa9a489438b618725d2372ab65264365ae9253248cd9  vendor/tiktoken-rs/src/api.rs
919af8a94d0df0fb887326ed57748655ad1dfb24140645e4f7f1cfb046eeca74  vendor/tiktoken-rs/src/lib.rs
39b4427528d501385247cd26364d678274819600b8bbb6e73c9c697bba513560  vendor/tiktoken-rs/src/model.rs
689ab8b5968b8ef7012dfe74d438dfe6efc7d6399ac0ab1227f6308516377005  vendor/tiktoken-rs/src/patched_tiktoken.rs
75188e2ae01a779000b7c42540c350dc21d9a0a83cecb3795a3f915e2116849d  vendor/tiktoken-rs/src/singleton.rs
b5f67c23b55ec827cf4f5f84b514e0d0a24207039cb1c4b0f16d6a83da92bbcb  vendor/tiktoken-rs/src/tiktoken_ext/mod.rs
70652b5898ab44c961a2a531b09bf6a0103a79cd92e6b3fc937bc23c44842b59  vendor/tiktoken-rs/src/tiktoken_ext/openai_public.rs
63cf6512160ae085ec45c84c1b4a74c4876f935570322e5cf8bb8e8ecafbb47c  vendor/tiktoken-rs/src/tokenizer.rs
18abedd3b8fde3c7036c61f87a9dec7b3bf31ebaa5cf1819be422385faef5ea6  vendor/tiktoken-rs/src/vendor_tiktoken.rs
```

此后改动走本仓提交链。


## pilotdeck-yaml：npm yaml 2.9.0 解析与组合移植（第 3 轮）

来源包：`resources/PilotDeck/node_modules/yaml/`（本机只读实路径 `/Users/da/ws/pilotdeck-rs/resources/PilotDeck/node_modules/yaml/`）。基线仓 commit：`cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`；package.json 版本 **2.9.0**；Node oracle **v22.22.0**。没有下载额外语料或依赖，生产实现不调用 Node。

移植保留 lexer/parser/composer 的函数、分支及状态机顺序。JS generator 的 yield 用输出队列表示，Rust 所有权通过局部取出/放回处理；节点和转换结果使用索引 arena 保留 alias 身份与环。循环 merge 在 Rust 中显式检测，并返回原版 toJSON 的 RangeError/message，避免无法恢复的原生栈溢出。动态字符串使用 `pilotdeck_protocol::JsString` 的 UTF-16 code units，位置也以 UTF-16 计数。静态 tag/code/version 常量用 Rust 字符串。regex/chrono/ryu-js 和测试 sha2 均复用本 workspace 已锁版本，无新第三方包。

以下表格逐文件登记实际范围；没有用空壳文件冒充未移植的编辑 API。公共 stringify、AST 创建/编辑、reviver、自定义 schema、compat 等留在明确未支持的边界。`cst-scalar` 的读取路径已实现，创建/替换标量是 stringify 后续工作。V8 的栈耗尽点依赖运行时优化与堆栈，未宣称极限栈深度和 Rust 完全相同；固定语料覆盖嵌套深度至 512。`stack.rs` 是 Rust 运行时适配：深集合的组合与较大值图转换使用临时 32 MiB scoped worker 栈，保留递归函数结构；外部 Rc LineCounter 不跨线程。2 MiB 调用线程中的 512 层输入（包括 keepSourceTokens）另行回归。

### 文件对应表

| npm yaml 2.9.0 文件（相对包根） | Rust 对应（相对 `pilotdeck-yaml/src/`） | 范围 |
|---|---|---|
| `dist/compose/compose-collection.js` | `compose/compose_collection.rs` | 解析路径移植 |
| `dist/compose/compose-doc.js` | `compose/compose_doc.rs` | 解析路径移植 |
| `dist/compose/compose-node.js` | `compose/compose_node.rs` | 解析路径移植 |
| `dist/compose/compose-scalar.js` | `compose/compose_scalar.rs` | 解析路径移植 |
| `dist/compose/composer.js` | `compose/composer.rs` | 解析路径移植 |
| `dist/compose/resolve-block-map.js` | `compose/resolve_block_map.rs` | 解析路径移植 |
| `dist/compose/resolve-block-scalar.js` | `compose/resolve_block_scalar.rs` | 解析路径移植 |
| `dist/compose/resolve-block-seq.js` | `compose/resolve_block_seq.rs` | 解析路径移植 |
| `dist/compose/resolve-end.js` | `compose/resolve_end.rs` | 解析路径移植 |
| `dist/compose/resolve-flow-collection.js` | `compose/resolve_flow_collection.rs` | 解析路径移植 |
| `dist/compose/resolve-flow-scalar.js` | `compose/resolve_flow_scalar.rs` | 解析路径移植 |
| `dist/compose/resolve-props.js` | `compose/resolve_props.rs` | 解析路径移植 |
| `dist/compose/util-contains-newline.js` | `compose/util_contains_newline.rs` | 解析路径移植 |
| `dist/compose/util-empty-scalar-position.js` | `compose/util_empty_scalar_position.rs` | 解析路径移植 |
| `dist/compose/util-flow-indent-check.js` | `—` | compat 专用；基线未使用且选项明确拒绝 |
| `dist/compose/util-map-includes.js` | `compose/util_map_includes.rs` | 解析路径移植 |
| `dist/doc/Document.js` | `doc/mod.rs + lib.rs` | 解析构造、directives/schema、toJSON；编辑/clone/toString/reviver 未公开 |
| `dist/doc/anchors.js` | `doc/anchors.rs` | anchorIsValid；创建节点时自动命名 anchor 不属于解析路径 |
| `dist/doc/directives.js` | `doc/directives.rs` | add/atDocument/tagName；tagString 在 nodes/stringify_key.rs |
| `dist/errors.js` | `errors.rs` | YAMLParseError/YAMLWarning 与 UTF-16 prettifyError |
| `dist/nodes/Alias.js` | `nodes/alias.rs` | resolve/toJSON/getAliasCount；key coercion 的 toString 在 stringify_key.rs |
| `dist/nodes/Collection.js` | `nodes/mod.rs` | 解析需要的集合类型；路径编辑接口未公开 |
| `dist/nodes/Node.js` | `nodes/node.rs` | 解析元数据；clone/路径编辑未公开 |
| `dist/nodes/Pair.js` | `nodes/pair.rs + nodes/add_pair_to_js_map.rs` | Pair 数据与 toJSON；key coercion 在 stringify_key.rs |
| `dist/nodes/Scalar.js` | `nodes/scalar.rs + nodes/to_js.rs` | Scalar 数据、类型与 toJSON |
| `dist/nodes/YAMLMap.js` | `nodes/to_js.rs + nodes/stringify_key.rs` | 映射 toJSON / hasAllNullValues；编辑接口未公开 |
| `dist/nodes/YAMLSeq.js` | `nodes/to_js.rs` | 序列 toJSON；编辑接口未公开 |
| `dist/nodes/addPairToJSMap.js` | `nodes/add_pair_to_js_map.rs` | 对象/Map/Set 键、合并、复杂键转换 |
| `dist/nodes/identity.js` | `nodes/mod.rs + nodes/node.rs` | enum 匹配代替 JS identity symbols |
| `dist/nodes/toJS.js` | `nodes/to_js.rs + doc/anchors.rs` | toJSON 的 keep=false 路径与 alias 身份；原生 Date/Buffer 的 keep=true 未公开 |
| `dist/parse/cst-scalar.js` | `parse/cst_scalar.rs` | resolveAsScalar；createScalarToken/setScalarValue 随 stringify 后续实现 |
| `dist/parse/cst-stringify.js` | `parse/cst_stringify.rs` | 解析路径移植 |
| `dist/parse/cst-visit.js` | `parse/cst_visit.rs` | item 遍历、控制返回、路径查询；Rust 根参数为 CollectionItem |
| `dist/parse/cst.js` | `parse/cst.rs` | 解析路径移植 |
| `dist/parse/lexer.js` | `parse/lexer.rs` | 解析路径移植 |
| `dist/parse/line-counter.js` | `parse/line_counter.rs` | 解析路径移植 |
| `dist/parse/parser.js` | `parse/parser.rs` | 解析路径移植 |
| `dist/public-api.js` | `lib.rs` | parseDocument/parseAllDocuments、默认选项与提前停止；parse/stringify 包装未公开 |
| `dist/schema/Schema.js` | `schema/mod.rs` | 解析 schema；构造/clone/自定义 schema 选项不公开 |
| `dist/schema/common/map.js` | `schema/tags.rs` | 解析标签，节点构造/修改留到 stringify |
| `dist/schema/common/null.js` | `schema/common/null.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/common/seq.js` | `schema/tags.rs` | 解析标签，节点构造/修改留到 stringify |
| `dist/schema/common/string.js` | `schema/common/string.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/core/bool.js` | `schema/core/bool.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/core/float.js` | `schema/core/float.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/core/int.js` | `schema/core/int.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/core/schema.js` | `schema/core/schema.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/json/schema.js` | `schema/json/schema.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/tags.js` | `schema/tags.rs` | core / yaml-1.1 / json / failsafe 标签注册与 knownTags |
| `dist/schema/yaml-1.1/binary.js` | `schema/yaml_1_1/binary.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/yaml-1.1/bool.js` | `schema/yaml_1_1/bool.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/yaml-1.1/float.js` | `schema/yaml_1_1/float.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/yaml-1.1/int.js` | `schema/yaml_1_1/int.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/yaml-1.1/merge.js` | `schema/yaml_1_1/merge.rs + schema/tags.rs` | merge 识别、alias 解引用、Map 合并优先级 |
| `dist/schema/yaml-1.1/omap.js` | `schema/yaml_1_1/omap.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/yaml-1.1/pairs.js` | `schema/yaml_1_1/pairs.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/yaml-1.1/schema.js` | `schema/yaml_1_1/schema.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/yaml-1.1/set.js` | `schema/yaml_1_1/set.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/schema/yaml-1.1/timestamp.js` | `schema/yaml_1_1/timestamp.rs` | 解析标签/resolve；仅 key coercion 所需 stringify 分支 |
| `dist/stringify/foldFlowLines.js` | `nodes/fold_flow_lines.rs` | 复杂键 flow / quoted 折行 |
| `dist/stringify/stringify.js` | `nodes/stringify_key.rs` | 仅 toJSON 复杂键所需 flow stringify / props |
| `dist/stringify/stringifyCollection.js` | `nodes/stringify_key.rs` | 仅复杂键 flow collection 分支 |
| `dist/stringify/stringifyNumber.js` | `nodes/stringify_key.rs` | 仅复杂键 number 分支 |
| `dist/stringify/stringifyPair.js` | `nodes/stringify_key.rs` | 仅复杂键 flow pair 分支 |
| `dist/stringify/stringifyString.js` | `nodes/stringify_key.rs` | 仅复杂键 flow string 分支 |
| `dist/visit.js` | `doc/anchors.rs` | 仅 Alias.resolve 所需前序遍历；通用 AST 修改 visitor 未公开 |

### 原始来源 sha256

哈希针对本机原始 npm 包字节，而非移植后的 Rust 文件。此处列出所有使用的源码和包元数据/许可证；未修改来源。

```text
20b8b197cbd10dad245d45e463dfe58e4c8c25a47e24bc4256ad9ab58bf35683  yaml@2.9.0/package.json
5bba27375d93e9119f76c1015f7672cf9ad5f70952296e0842fb2243d6376869  yaml@2.9.0/LICENSE
a7db11a1a7987d6412df8adbbb85ccd4370171f82265703b2b6af2d13c45d646  yaml@2.9.0/dist/compose/compose-collection.js
0f388537747b5c1a241639413f17fc2b5f831451b5017d6902e46db6842a5fe4  yaml@2.9.0/dist/compose/compose-doc.js
86562c85e4e5737d28b24aae2030b97bc9b672e1f2de5da9e4d25c4571e4345c  yaml@2.9.0/dist/compose/compose-node.js
37920472755bdaa76399b6d33872f0015ae1fd9ba7faa031c03d501aa8c3e903  yaml@2.9.0/dist/compose/compose-scalar.js
509c0bdb4cef641cc784c130b6cc2b350752f6841798daa1a13cbcaf4fc74c46  yaml@2.9.0/dist/compose/composer.js
dcfada448c8fc532ebba96d6788a4b41bb015fc5e697a9544ff429b9c4e4c70a  yaml@2.9.0/dist/compose/resolve-block-map.js
af562e2e3cd7ee7b3cb9342984177f6b5c77de73121e87695db3ff635e12d152  yaml@2.9.0/dist/compose/resolve-block-scalar.js
18f54a39d73b600ebbd74a343e6671d0d08423f125bd86963b72dc24d6261911  yaml@2.9.0/dist/compose/resolve-block-seq.js
532bf56299183e75bb7c20db75321367c28393b66ecb8ffd540faedd9817f116  yaml@2.9.0/dist/compose/resolve-end.js
242470b54c0deba1507babba2e879f3005ed2ee3bd53a36affb6ecf82fddd375  yaml@2.9.0/dist/compose/resolve-flow-collection.js
167fa296e6fb75618bb7b315acaa926d4e88b2b63a9ff139f1a7de9c78db4610  yaml@2.9.0/dist/compose/resolve-flow-scalar.js
0b49c6af0145c309883dbf24d1a00260c0c090412be10aaa301de126dad15983  yaml@2.9.0/dist/compose/resolve-props.js
60182618fc9fa669e74ab74f4231c15b5d24bc52ab0a419684bb827a00d69be6  yaml@2.9.0/dist/compose/util-contains-newline.js
460516395ef7e961d5248e047ee6d52084f279b76e8af9f3c2bac92da1c7d656  yaml@2.9.0/dist/compose/util-empty-scalar-position.js
c7c0c9837a1f828de19eb3ee0947dff39c4f69222c762aed9fa2fe14fc46ee3e  yaml@2.9.0/dist/compose/util-flow-indent-check.js
0330dc34072cb499493176b293228631ca8ebaaaddcdc0e63b02bf24f6a25c2d  yaml@2.9.0/dist/compose/util-map-includes.js
9bc1c9a56de3902d554e77193125b6aadcfaec743073b3417b22c95a8f7d7923  yaml@2.9.0/dist/doc/Document.js
ea9a479b1e98d88235ffa6736d0405e2119e9dde7b5249b72185488ab2527261  yaml@2.9.0/dist/doc/anchors.js
8f02c3c03571e377e33680b68027d4336234d7a6da3d8c6a8b7f1183c1c428ff  yaml@2.9.0/dist/doc/directives.js
c389d9c448193ce1bcb9641f5eda62cbe56467b2104357d2fc8dd86df88cb07b  yaml@2.9.0/dist/errors.js
80a8109b7cb1d0d73eee18463fdd3095d120fa21f49b282444d90d867e291ea1  yaml@2.9.0/dist/nodes/Alias.js
e2d3ffdc9a997774602e72b296fba641558bc7355c9f12a3ee0502fb1e541306  yaml@2.9.0/dist/nodes/Collection.js
b57b42071f7ebab633626858656ec5187617f2290c272e87edc57467ef591215  yaml@2.9.0/dist/nodes/Node.js
d5dad67916ba0e864a892da63265a062c45c15a191349ef50a540f2ddbae93e5  yaml@2.9.0/dist/nodes/Pair.js
616ac0f6a0a5a6565307a4bb4443691b0dd019c03d6bb8777353750bd3459487  yaml@2.9.0/dist/nodes/Scalar.js
d140cfc5aa8d28737bd3c993d977d5a10ab81efe221e36708ac45b3cf18f5dce  yaml@2.9.0/dist/nodes/YAMLMap.js
9f89417f3cba5f22a4436f67a9ff9dce627344b5874d2ed7957ff9acffb030e4  yaml@2.9.0/dist/nodes/YAMLSeq.js
e4977864d75c1fdbf7fc78c9f4329406e153deccfff090fd812dae2b27e6c2c2  yaml@2.9.0/dist/nodes/addPairToJSMap.js
1a7cb042ae725ff68d05dc503efbbecbe65281f18b23dfc23f12d1749a440cfe  yaml@2.9.0/dist/nodes/identity.js
34de93bb4e16abbf317210a7bd1b062ed20acb028cd8395b69b591f433e277b6  yaml@2.9.0/dist/nodes/toJS.js
2f72b634348b04b395408ac427fb4f8ee67432244c327561d4b755a28a783b34  yaml@2.9.0/dist/parse/cst-scalar.js
4d14a7a286cff9c1a578ab15839388ea76f77ae79293db25b235394bcc377b51  yaml@2.9.0/dist/parse/cst-stringify.js
525535fc05841407420be6505804c74d873eb3aaa9319679896f7a24a474ca01  yaml@2.9.0/dist/parse/cst-visit.js
acc25760d010068844f36f4aefc7fe6f5f6048fd7f8fcc29d4290e154819d1a4  yaml@2.9.0/dist/parse/cst.js
cb30a2c4628e270ee810530217f3f7bb0f2730ab89f8fbaf1f8a73ccd1883cf5  yaml@2.9.0/dist/parse/lexer.js
ddfd11ca2cdd42d36bc6c3090a31c21e6bbcc90bf3498bf6aacfbb30c5d79436  yaml@2.9.0/dist/parse/line-counter.js
0adc5ffa51ffa4c0ceed1f80efa8b188d10f58c7b9389a6d46bc861e6bff34e1  yaml@2.9.0/dist/parse/parser.js
9b318ac7b3a3751127c74f59dd1dd209230a70ca116000348b82faae189f72f0  yaml@2.9.0/dist/public-api.js
d3b1bc956a766e2489c36114366f3b3f89762a257ae05ff6271ee3fc8c2a9bf5  yaml@2.9.0/dist/schema/Schema.js
769207eaa56e34c113da10cdc22da845f92b4080ff2b921a72847a38d3ac03d8  yaml@2.9.0/dist/schema/common/map.js
f5d5f694ea3a42ef136e0fbcc674dfd7aa0a3da2b8d62183fb750166a12e52bb  yaml@2.9.0/dist/schema/common/null.js
51fc0e8bfc63d1d9c7c66b0370a069bb33e0dad865080ff76802803597b9909a  yaml@2.9.0/dist/schema/common/seq.js
51d0b3aafab2ffa4ad9d86656a3b92c4b4824d0a8806423882ea5f92a1bf8d11  yaml@2.9.0/dist/schema/common/string.js
9fa696b1efc38cf8c73a398f30d9f9841658aa5afd0b7a6b0822947c3991ba7a  yaml@2.9.0/dist/schema/core/bool.js
139e3964f0e626ad3741500faf51d855ff98eafa2d9e7523cac06260e4515382  yaml@2.9.0/dist/schema/core/float.js
f0e474254f6877d5dd23bb57017980931ab333a7eaa3d33872247a80d11f754d  yaml@2.9.0/dist/schema/core/int.js
481d7c64817b40090d519fb65bd83730dadd4c6baec1440b9bf85d29e52c046e  yaml@2.9.0/dist/schema/core/schema.js
9efdd848455f6e0621feb13f3eb762453cb02b09d5e04b6501163cfb26af27ed  yaml@2.9.0/dist/schema/json/schema.js
dba2f42d4dbe8cc8b13f662fd97d253da2da5567e71c47115305efede5a888f2  yaml@2.9.0/dist/schema/tags.js
15d7b0b8516c8190b191ec9a0adaab16808d09751ef534d7829512944072011f  yaml@2.9.0/dist/schema/yaml-1.1/binary.js
80a0046224dac3437f22e3b27b0180e1aad82277ab2fee4769094c6d12d67b6f  yaml@2.9.0/dist/schema/yaml-1.1/bool.js
a90c44db83152e4d902d46f667737dac90aa86b401abdefe89371ac3bd03abfe  yaml@2.9.0/dist/schema/yaml-1.1/float.js
5ff72b08fecc3a924d4a76658a304859db5edb4fadf8893ee837aa06bba60a52  yaml@2.9.0/dist/schema/yaml-1.1/int.js
9c85b5e026e4369a96f3f7b5bc6c05da102dfa2968950b17986201cfe0be542c  yaml@2.9.0/dist/schema/yaml-1.1/merge.js
3174270cd59c0f0e3ffd73b3b44c4de679efd5c681325b7aac8e0935d547bee8  yaml@2.9.0/dist/schema/yaml-1.1/omap.js
46ba76da72ad65a6202358c486c6a3084ab7c48a8eb5be60d241d9e198c5a9b5  yaml@2.9.0/dist/schema/yaml-1.1/pairs.js
a7320808484dc6d04817b33b2d09b65ead7ffc1c672ab54e068a80972b17702e  yaml@2.9.0/dist/schema/yaml-1.1/schema.js
e4553b0cbde79090884908f44bb3b4afd04935fc23656a1b87ec1af87f24427a  yaml@2.9.0/dist/schema/yaml-1.1/set.js
b157c946dc1b8854aadc4e7994b268ec8afe6d916721cb3580e0752c7d15a5f7  yaml@2.9.0/dist/schema/yaml-1.1/timestamp.js
7819679e80222b80c564ea4a9cf3145fa4d5e28a6cfbac248283ac55b0249761  yaml@2.9.0/dist/stringify/foldFlowLines.js
38efb5bc5553e98fee58c825a88131c59184368d8689e3e6f43c652a06e9c167  yaml@2.9.0/dist/stringify/stringify.js
b55fd3b6e3f1346bbcd1fcdc48a59c089b7a4b0e6fac2b46846f7828f9488600  yaml@2.9.0/dist/stringify/stringifyCollection.js
26a1980d34a21a80704d356d475a0187a24ebcbeb37a2e3b149d4d791bb65598  yaml@2.9.0/dist/stringify/stringifyNumber.js
bc03ae533d85bf9122e87c0a256c841ae7fbf063499bc3034d49fd9f7b200659  yaml@2.9.0/dist/stringify/stringifyPair.js
4113b71a9c4c087a197f71452794a6acb9929111f569c973ff4efe7caee8f777  yaml@2.9.0/dist/stringify/stringifyString.js
a757e3b924af184b25e2386d1678b1fe341805a516ebf9bccee219b47aebd5e7  yaml@2.9.0/dist/visit.js
```

### ISC 许可原文

```text
Copyright Eemeli Aro <eemeli@gmail.com>

Permission to use, copy, modify, and/or distribute this software for any purpose
with or without fee is hereby granted, provided that the above copyright notice
and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES WITH
REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF MERCHANTABILITY AND
FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT,
INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS
OF USE, DATA OR PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER
TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF
THIS SOFTWARE.
```

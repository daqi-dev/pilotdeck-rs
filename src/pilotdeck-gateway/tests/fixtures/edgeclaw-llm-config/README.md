# EdgeClaw LLM config resolution (A6.6-edgeclaw-5)

真相源为 `OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`（Node v22.22.0）：

- `service.ts:112-345`（`parseModelRef` / `resolveOpenClawModelConfig` /
  `buildLlmConfig` / `mergeIndexingSettings`）
- `createEdgeClawMemoryProviderFromConfig.ts:78-112`
- `core/skills/llm-extraction.ts:849-908,2162-2259`

`observe.mts` 用 tsx 的 esbuild 变换原文件后追加导出，直接调用真实 TS；
不复制函数体。每个 `buildLlmConfig` / openclaw case 都在独立临时 `HOME`
下写 `~/.openclaw/openclaw.json`，并按 case 设置/清理
`EDGECLAW_MEMORY_*` / `OPENAI_*` / `ENV_KEY` / `LOOKS_LIKE_ENV`，因此
oracle 与宿主环境无关。cache case 在同一个模块实例上先坏 JSON 再写合法
JSON，记录两次返回值，锁定「首次失败后永久 null」。

覆盖 366 个观测：openclaw 缺失 / 坏 JSON / primary 命中与不命中（含按
`id` / `name` 命中，且 name 命中的模型可以不是首个模型）/ provider
或 model 缺失 / headers 合并与 trim（数组 headers 被 `isRecord` 拒绝）/
整数 provider 名按 JS 键序回退（raw JSON 文本里 `10` 在 `2` 前）；
`buildLlmConfig` 的默认值、六条 env 回退、options 覆盖、modelRef、openclaw
选择命中与不命中、apiKey 环境变量名字面量；extractor 的 `sanitizeHeaders`（含数组记录）、`resolveAgentPrimaryModel`、
`parseModelRef`、`looksLikeEnvVarName`、`resolveSelection`（api/baseUrl/
headers 覆盖与尾斜杠）、`resolveApiKey`（直接值 / 环境变量名 / runtime
resolver）、`mergeIndexingSettings`（含 NaN / Infinity / -0 语义）与工厂
`resolveMemoryLlm` / `memoryApiTypeForProtocol`。

生成两次并核字节（从仓库根目录运行）：

```sh
export PDRS_BASELINE=/absolute/path/to/fixed/PilotDeck
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-config/generate.mts /tmp/edgeclaw-llm-config-1
node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-config/generate.mts /tmp/edgeclaw-llm-config-2
cmp /tmp/edgeclaw-llm-config-1/oracle.json /tmp/edgeclaw-llm-config-2/oracle.json
```

测试：

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-gateway --test edgeclaw_llm_config
PDRS_BASELINE="$PDRS_BASELINE" cargo test --manifest-path src/Cargo.toml \
  -p pilotdeck-gateway --test edgeclaw_llm_config -- --include-ignored
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-config/negative.py /tmp/edgeclaw-llm-config-negative
python3 src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-config/negative.py --self-test
```

`negative.py` 用 `mutating_sources` 锁定并恢复产品源码，逐格要求编译成功、
运行时断言转红；一格失败不中止，跑完后汇总 `KILLED / SURVIVED / INVALID`，
有存活或无效退出 1；`SURVIVED` 还要求恰好跑满 1 个测试，测试名漂移导致的
`running 0 tests` 记 `INVALID`。脚本要求调用方提供 `CARGO_TARGET_DIR`，未设
即报错。`--self-test` 直接校验分类器的三态。整套负控逐个重编译，耗时较长，
由协调者在宿主执行（见 RESULT）。

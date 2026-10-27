# External skills management port fixtures (#929)

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。
被测源码（SHA-256 在 `oracle.json.sourceSha256`）：

- `src/composition/domainPorts.ts` — `createSkillManagementPort`（75–105）
- `src/composition/HttpModuleClient.ts` — manifest / `module_call` / 错误投影
- `src/composition/types.ts` — `MODULE_HTTP_TRANSPORT`
- `src/agent/modules/protocol.ts` — `validateModuleMessage` 响应分支

生成器启动一个 `node:http` 本地 module host（`127.0.0.1:0`），隔离的
`HOME` 指向 scratch 下的 `home/`；对八个管理方法各跑三类：

1. **success**：`ok:true` + 对象 `payload.result`；
2. **undeclared**：binding.methods 不含该 operation（不发 HTTP）；
3. **provider-error**：`ok:false` + `code/error{code,message,retryability,details}`。

另录 `missing-credential`（port 自身的未配置分支：manifest 后、POST 前失败）、
`manifest-missing-method`、`result-not-object`、`http-500`、`invalid-json`、
`correlation-mismatch`、`ok-with-failure-fields`。

每例录下真实 TS 发出的全部 HTTP 请求（method / path / content-type /
authorization / body）和真实 HTTP 响应（status / body），Rust 的脚本化
transport 逐字节比对请求后回放响应，再逐字节比较结果或错误形状。

归一化：只有 `endpoint` 里的随机端口写成 `<port>`；请求只比较 path，
不比较 host/port。`input` 是固定字面量，不含机器路径。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
env -u LC_ALL -u LC_MESSAGES LANG=en_US.UTF-8 \
  node --disable-warning=ExperimentalWarning \
  --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
  src/pilotdeck-gateway/tests/fixtures/skills-external/generate.mts <scratch>
```

Rust 默认测试 `skills::external_tests::external_skills_oracle_replays_requests_and_outcomes`
重放 `oracle.json`；`#[ignore]` 测试
`skills::external_tests::external_skills_fixture_regenerates_identically_from_ts`
重生成并逐字节比较。TS config 只允许 `credentialEnv` 用于 knowledge；
`missing-credential` 测的是 `createSkillManagementPort` 适配器自身的分支，
不代表 skills 配置会走到这里。

负控：`negative_controls.py` 只变异产品源码（`skills/binding.rs`、
`skills/external.rs`），用 `negative_suite.run_cases` 逐项汇总
KILLED/SURVIVED/INVALID；锚点过期在 `--check-anchors` 预检直接退出 2。
因新增/修改文件在提交前不等于 HEAD，`mutating_sources` 会按设计拒绝；
脚本在提交后的宿主运行：

```sh
CARGO_TARGET_DIR=/path/to/target python3 \
  src/pilotdeck-gateway/tests/fixtures/skills-external/negative_controls.py \
  _tmp/<date>-issue-929/negative-controls
```

# Session overrides / permission registry TS fixtures

固定基线：`OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。三个被测源码的 SHA-256 记在 `oracle.json.sourceSha256`：

- `src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts`
- `src/gateway/permission/GatewaySessionPermissionModeRegistry.ts`
- `src/always-on/runtime/SessionConfigOverrides.ts`

所有 outcome、registry `size`、会话 key 的 allow 快照、live lease 的完整 rule set、mode 的 current/base、override 的 get 结果都由真实 TS 类产生；Rust 测试不写死期望值。

```sh
export PDRS_BASELINE=/absolute/path/to/PilotDeck
node --expose-gc --experimental-strip-types \
  src/pilotdeck-gateway/tests/fixtures/session-overrides/generate.mts \
  src/pilotdeck-gateway/tests/fixtures/session-overrides
cd src
cargo test --locked --offline -p pilotdeck-gateway --test session_overrides
```

生成器先断言基线 HEAD、三个源文件无工作区改动，并记录源文件哈希。同一命令连跑两次应得到逐字节相同的 `oracle.json`（无时间戳、路径或随机值）。

## 覆盖

`ruleSet`（53 步）：未知会话 `allowRules` / `updateAskRules`；创建时保留 configured 重复项；`acquire` 时把 configured 去重合并进同一组数组；`force` 不同但 `sameRule` 四字段相同视为重复；`pattern` 不同视为新规则；显式 `pattern: undefined` / `force: undefined` 作为合法输入编码一次并参与同一 `sameRule` 比较；两个 lease 重叠、release 幂等、最后一个 release 删除无 pin 的 entry；`grant` 先于 AgentHandle 时 pin entry，`closeSession` 解除 pin 并按 retain 删除；`dispose` 清空后 lease 仍持有原数组；空字符串与孤立代理 sessionKey；非法 source/behavior 原样存储。

`mode`（31 步）：未知 key 的 `get`/`getBase`；`plan` transition 的 `baseBeforePlan ?? current ?? fallbackBase` 顺序；重复 plan 保持 base；非 plan transition 清 base；`set` 清 base；`clear`/`dispose`；非法 mode 字符串原样存储；空字符串与孤立代理 key。

`overrides`（39 步）：同 key 替换而非合并；未知顶层属性保留；`null`/`undefined`/数字/字符串/数组的 object spread 语义（字符串与数组按 UTF-16 索引展开）；`get` 返回顶层副本（修改返回值或 set 的输入不改变存储）；`delete`/`deletePrefix`/`clear`；空字符串与孤立代理 key。

`identity`（61 步）：逐条钉住 `permissionRules` 的对象/数组成员身份。codex 第二轮的两条失败序列都在内：① `permissionRules: {}` → `acquire` → `grant(Read)` → `updateAsk` → `close` → `release` → `acquire`，重建后三个成员都是新数组；仅声明 `allow` 时 grant 保留、缺省 `ask` 重置；三个都声明时二者都保留；显式 `undefined` 等同缺省。② 跨 `SessionConfigOverrides` 实例 `B.set("c", A.get("a"))` 共享 A 的对象而不是 B 的首个句柄；同一个已取得视图连续两次 `set` 仍指向同一对象（中间的 grant 保留）；`delete` 后把旧视图设到新 key 仍保持身份与活数组。`sameStores` / `sameHeld` 记录 TS 的引用比较结果，Rust 侧用 `ConfiguredRuleSet::same_object` 回放。

`combined`（54 步）：把 `SessionConfigOverrides` 与规则 registry 按真实 factory 的用法组合。`set` 含空 `permissionRules` 的 override → `acquire` → `grant` → override `get` 可见 → `close` → `release` → 从同一 override 重建 → 仍读到该 grant；`updateAskRules` 同时到达两个 lease 与 override 视图；`holdAllow` 在 `grant` 前取得 `allowRules` 引用、`readHeldAllow` 在 `grant` 后读取；`setFromGet` 复刻 `{ ...existing }` 的 cwd/mode 更新路径，更新后新 grant 在重建时仍可见；`dispose` 后同 key 重建、旧 lease release 不删除新 entry；部分 `permissionRules` 只暴露它声明的数组成员；显式 `undefined` 可选字段经过 override → handle → registry 后仍按缺省参与 `sameRule`。

## oracle 形状

```text
ruleSet[i]  = { step, outcome, size, state: { allow: {key: rules[]}, leases: {label: fullRuleSet} } }
mode[i]     = { step, outcome, state: {key: {current, base}} }
overrides[i]= { step, outcome, state: {key: storedObject | undefined} }
combined[i] = { step, outcome, size, state: { overrides: {key: object|undefined}, allow: {key: rules[]}, held: {view: rules[]}, leases: {label: fullRuleSet} } }
identity[i] = { step, outcome, size, state: { stores: {"store:key": object|undefined}, allow: {session: rules[]}, leases: {label: fullRuleSet} } }
```

JSON 没有 `undefined`/`NaN`/`Infinity`/`-0`，编码为单键对象 `{"$":"undefined"}` 等；真实 fixture 对象不允许使用保留键 `$`。会话 key 与规则字段保留 UTF-16 码元。生成器只在记录最外层编码一次，显式 `undefined` 可选字段不会被二次编码。

## 第四轮：对象图与回收

`sharing`（81 步）从真实 TS 录制顶层副本、原始输入重复 set、内层 object spread、permissionRules / excludeTools / 未知嵌套对象 / rule 元素的身份与变更传播。包括 lease 修改缺省 deny → 同代 replacement 可见 → 最终 release → 新 generation 清空；configured 成员重新赋值；lease rules 成员重新赋值；删除活 allow 内容；已有 entry 合并只共享 rule 元素而不采用 incoming 数组；三个成员共用同一数组；未知 key 每次返回新数组；dispose 后旧 release 不删除新 entry。每步比较 outcome、size 和全部持有值、override、lease 的快照，`same` 的期望来自 TS `===`。

`lifetime` 由 Node `--expose-gc` + WeakRef 实测：replace / delete / deletePrefix / clear / drop 五类各创建 100 份配置。前四类在 **store 仍存活** 时验证历史数组回收，drop 类验证实例销毁。每轮 GC 前先让出事件循环，避免 WeakRef 当前 job 保活；有界重试仍未回收则生成失败。Rust `session_value::tests::override_churn_releases_every_historical_array` 用 Weak 对照这些结果；另测视图、lease、allow handle 最后一个持有人释放后的回收。

`modeIdentity` 只记录 Node 对模式 registry 内部对象的观察：set / transition 新建内部 entry，get 是字符串标量；公开返回值继续由 `mode` 的 31 步回放验证。

## 共享接口与范围

只移植三个纯内存类，不接入 host、session runtime 或 gateway method。`SessionValue` 表示会话配置的无环对象 / 数组图；`clone` 保留身份，`spread` 新建顶层容器，`to_value` 才生成脱离共享的序列化快照。从 `JsValue` 导入一次后应继续传递句柄，不能把快照反复导入当成原对象。`SessionConfigOverrides::set/get` 均执行浅复制，所有嵌套引用都保留；没有原型编号、进程全局表或实例内历史表。

`ConfiguredRuleSet` 指向配置对象；`RuleSetHandle` 指向 registry 每代新建的 rules 对象，同代 lease 共享它。首次 acquire 逐成员采用已有数组，对 missing / undefined / null 新建缺省数组；已有 entry 只按 sameRule 合并元素引用，不替换数组。规则对象、配置属性和 lease 属性各自的赋值/修改影响范围由真实对象身份决定。

生产调用方用 `lease.rules().update_member("deny", |rules| ...)` 修改、删除或替换数组元素；通过 `lease.rules().value().set("allow", array)` 重新赋值成员。旧的 allow handle 在成员重新赋值后仍持原数组。回调不得重入同一个数组或使用该数组的 registry；快照逐节点读取，不同时持有多把数组锁，支持 allow/deny/ask 共用一个数组。

`runtimeBoundary` 另外保留 r1 评审的越类型 Node 探针：首次 grant(null) 成功，第二次报 TypeError 及原始文本。它明确不计入 Rust 类型域的回放通过数。

这不是通用 JS 运行时：输入范围是 TS 类型约定的规则和无环数据配置（保留已有非法枚举字符串与顶层 primitive spread 用例），不实现 getter / Proxy / 循环引用或任意畸形规则触发的 JS 异常。`PermissionRule` / `PermissionRuleSet` 是脱离共享的便利构造与快照；真实夹具比较走句柄 `to_value`，保留显式 undefined 和未知字段，不再归一化删除 pattern/force 的 undefined 属性。

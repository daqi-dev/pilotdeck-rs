# #778 默认工厂与 M21

Node 必须是 v22.22.0，`PDRS_BASELINE` 指向 cc7c4e6 的只读 checkout。
`generate.mts`/`verify.mts` 用 bindBaseline 绑定全部源树，并记录本入口依赖闭包。
`observe.mts` 用 Node resolve hook 将 bare `edgeclaw-memory-core` 定位到基线
`src/index.ts`；factory、bundle、controller 均直接 import，并断言 service
是同一 source 类的实例。没有加载 lib，也没有复制或修改基线源码。

生成器参数是调用方新输出目录：

```sh
PDRS_BASELINE=/abs/PilotDeck node --disable-warning=ExperimentalWarning \
  --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-native-default/generate.mts NEW_OUTPUT
```

输出 `oracle.json` 使用 `wx`，`ts-store/` 也必须尚不存在。该目录含本轮真实
E2E 写入的 SQLite 与文本记忆树；Rust 在 **原目录布局和同一 workspace 路径**
打开它即可做 TS→Rust。布局为 `old/workspace`、`old/store/workspaces/<sha1>/`，
其中 sha1 是绝对 workspace 路径的 SHA-1 前十位，绝不可从 oracle `$HASH`
当成真实路径。

Rust→TS：Rust 先在独占目录按同样布局完成默认工厂 retrieve→capture→scheduled
maintenance→retrieve，然后真实 TS 读取它：

```sh
PDRS_BASELINE=/abs/PilotDeck node --disable-warning=ExperimentalWarning \
  --import /abs/PilotDeck/node_modules/tsx/dist/loader.mjs \
  src/pilotdeck-gateway/tests/fixtures/edgeclaw-native-default/verify.mts \
  --read-root RUST_STORE_ROOT NEW_OUTPUT.json
```

该模式首先比较打开前的业务表，防止构造器补写 workspaceDir 掩盖缺数据；
再经真 factory 重开并比较完整业务表、文件文本和 get 返回，再次 retrieve
必须命中。输出文件使用 `wx`。不带 `--read-root` 时参数是 Rust 全量观测 JSON，
与新鲜 TS 的 scenarios/configurations/endToEnd/maxLines 全量比较。

M21 的 13 格由 `M21` 数组静态列全。LLM await 使用 fetch gate；全部等待有
10 秒截止。never 格在旧 gate 未释放时确认 dispose 和同 key 新代际维护完成，
之后放行只为了探针清理。该格不宣称生产取消了旧请求。热重载格使用相同
projectRoot、不同配置 rootDir，真实两代 runtime 对象进入同一个 controller。

配置 24 格覆盖缺失/禁用/provider、模型优先级/协议、rootDir（含孤立代理项和
真实 ENOTDIR）。E2E 5 次 HTTP 请求记录原 body 文本，持久化比较包含所有非
sqlite_ 内部表、workspace/global memory 文本树。只归一化已绑定的root/hash/HTTP
origin；保留其余业务字段、原始JSON文本与时刻。Rust真实双向运行和 ignored
重生成守卫的最终结果写在任务证据 RESULT.md，生成器本身不替代这些验收。

默认 bundle 无注入发布句柄的生产链由
`memory_native_default_published_end_to_end` 另行守护。配置错误矩阵还覆盖
损坏 SQLite / SQLite 路径为目录：比较错误 name/code/message 和失败时的
memory/global 文件树，确认数据库失败先于记忆文件生成。

#1052 的 `maxLines` 金样使用 81 行真实记录，分别调用省略参数、显式 undefined、
null；Rust `service_get_maxlines_defaults_1052` 逐项比较内容。

`negative.py` 复用 `_shared/mutant_switch.py`，一次编译全部分支，执行每个
选中变异的正控/负控/恢复正控并按 `negative_suite` 汇总；不写源码、不改 HEAD。
调用方必须提供 CARGO_TARGET_DIR；输出目录必须新建。作者会话仅运行
`native-default-empty` 样本，其余已编译但待协调者批跑。

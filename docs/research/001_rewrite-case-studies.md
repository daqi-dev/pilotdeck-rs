# 001 · 真实重写/移植案例：对 PilotDeck TS→Rust 的启示

> 2026-10-13。由一个调研 agent 读一手来源整理，主会话抽查过一条（tsgo `testdata/submoduleAccepted.txt`
> 确实存在，2026-10-13 为 1539 行，按特性分组；`CHANGES.md` 存在）。其余条目**未逐条复核**，引用前回原文看。
> 题号：Q1 等价粒度 · Q2 烂架构照搬还是重设计 · Q3 原代码 bug 兼容还是修 · Q4 测试/探针本身错 · Q5 渐进还是一次切换 · Q6 非确定性。

## 一、案例

| 案例 | 一句话 | 关键做法 | 题 | 来源 |
|---|---|---|---|---|
| TypeScript 编译器 → Go（tsgo） | 明说是 port 不是 rewrite，刻意保留结构 | 原仓作 submodule，跑原测试与 baseline 对比出 `.diff`；分 `submoduleAccepted`（已接受）/`submoduleTriaged`（已分诊）；有意改动记 `CHANGES.md`。团队自己说：需要重想内存/可变性的语言（Rust 属此类）更适合重写而非移植 | Q1–Q4 | github.com/microsoft/typescript-go/discussions/411 · 仓库 `CHANGES.md` |
| fish C++ → Rust（4.0） | 逐组件渐进，每一步都有能用的 fish | autocxx 桥接；保留结构便于逐段对照；端到端测试全程可跑是选渐进的关键；估半年，实际近两年，大头在「第二个 90%」；不少错误 `unwrap` 导致崩溃 | Q1 Q2 Q5 | fishshell.com/blog/rustport/ |
| Turborepo Go → Rust | 第一次方案（静态链接 Go）在 musl 上失败，改成两个二进制 | 先补测试把行为变规格；「Rust-Go-Rust 三明治」双路径跑、hash 不一致即 CI 失败；序列化换了三次（JSON→Protobuf→Cap'n Proto）；72 小时 canary；教训：别边移植边重构，先把核心行为写成文档 | Q1 Q2 Q4 Q5 | vercel.com/blog/how-turborepo-is-porting-from-go-to-rust · …/finishing-turborepos-migration-from-go-to-rust |
| Figma 多人服务器 TS → Rust | 放弃整体重写，只迁热点 | 网络层留在 Node，每文档一个 Rust 子进程，stdin/stdout 消息协议作边界 | Q1 Q5 | figma.com/blog/rust-in-production-at-figma/ |
| Dropbox Nucleus | 增量改不动数据模型才重写，并明说换个场景就是坏主意 | 测试为确定性设计：单线程核心、种子 PRNG、去 HashMap 随机化、mock FS/网络/时钟、自动缩小失败用例；不变量要强到挡住「空树也算已同步」；另有慢 100 倍的真协议测试兜底 mock 偏差 | Q2 Q4 Q6 | dropbox.tech/infrastructure/rewriting-the-heart-of-our-sync-engine · …/-testing-our-new-sync-engine |
| Discord Read States Go → Rust | 只为 GC 尖刺，针对性重写 | 先粗译再简化；压测 → 单节点 canary（抓到漏掉的边界情况）→ 全量；没有差分测试 | Q2 Q5 | discord.com/blog/why-discord-is-switching-from-go-to-rust |
| Cloudflare FL1 → FL2 | 新系统没写完就先接流量 | 新模块先嵌进旧系统；FL2 处理不了的请求原样转给 FL1；端到端测试同时打两边；分阶段灰度 + 自动回滚 | Q1 Q5 | blog.cloudflare.com/20-percent-internet-upgrade/ |
| GitHub Scientist | 生产里新旧并跑，只返回旧结果 | 不一致记日志；时间戳不一致 → 把时间改成显式参数；抓到旧代码 bug（退出码对 256 取模变 0）后**修了而不是复刻**；只适合无副作用代码 | Q3 Q4 Q6 | github.blog/engineering/engineering-principles/move-fast/ · github.com/github/scientist |
| Twitter Diffy | 两个旧实例互比找出噪声字段，再和新实例比时忽略这些字段 | 「旧 vs 旧」的差异就是噪声 | Q6 | infoq.com/news/2015/09/twitter-diffy（二手报道） |
| Prisma Rust → TS（反例） | 从 Rust 迁回 TS | 跨语言序列化开销、多平台构建、贡献者门槛；收益主要来自去掉边界 | Q5 | prisma.io/blog/from-rust-to-typescript-a-new-chapter-for-prisma-orm |
| Spolsky《Things You Should Never Do》 | 旧代码里沉淀着边界情况的修复 | 重写会丢掉它们，而且长期没有可发布版本 | Q2 Q3 | joelonsoftware.com/2000/04/06/things-you-should-never-do-part-i/ |
| Google LLM 迁移（2025） | 约 80% 改动由 AI 生成 | 构建/测试失败回喂；AST 检查拦多余改动；人审是瓶颈 | Q4 | arxiv.org/abs/2501.06972 |
| Airbnb Enzyme→RTL | 3500 文件 6 周 | 每文件一个状态机、每步有门禁；门禁只证明「能跑」，不证明语义等价 | Q4 | airbnb.tech/?p=366 |

## 二、结论之间的冲突，以及对本项目的含义

1. **照搬还是重设计（Q2）。** tsgo、fish 照搬结构以便对照抓错，Turborepo 说别边移植边重构；但 tsgo 自己也说 Rust 这类语言更适合重写。⇒ **边界照搬，内部重设计**：模块边界和数据流按原样映射，模块内部按 Rust 习惯写。要重构就在移植前或移植后做，不在移植过程中做。
2. **等价定在哪一层（Q1）。** 所有案例都比**外部可观察面**：baseline 输出、hash、端到端请求、返回值。⇒ 本项目三类契约：**HTTP/WS 协议 · 落盘格式（会话、记忆）· 工具与 MCP 的 I/O**；函数级只用于少数纯逻辑模块。
3. **bug 兼容还是修（Q3）。** Spolsky、Turborepo 偏兼容；Scientist、tsgo 修了，并且记账。⇒ **默认在可观察面上 bug 兼容**；每一处修复进**接受差异清单**并写明原因（对应 tsgo 的 `submoduleAccepted.txt`）。
4. **测试/探针错了（Q4）。** ⇒ 期望值由 TS 基线生成后锁定，**AI 不得直接改**。改期望值要分诊（接受 / 待定 / Rust 侧 bug），由人拍板。两边都返回空结果或都报错，**不算一致**。
5. **渐进还是一次切换（Q5）。** 渐进占多数，但进程内跨语言边界代价高（Turborepo、Prisma）。⇒ **不做进程内 FFI**。照 Cloudflare 的做法：Rust 网关站在前面，没实现的路由和 WS 消息原样转给 TS 后端，逐条路由切过去。
6. **非确定性（Q6）。** ⇒ 时钟、ID、随机种子都可注入；LLM 调用录制 + 回放；先让 TS 跑两遍找出噪声字段（Diffy 的做法）；有副作用的操作在隔离目录里跑，比终态。
7. **工期。** fish 半年变近两年，Turborepo 7 万行用了 15 个月。18 万行一个人做，即使有 AI，也要假设长尾很长；**审查和分诊差异会是瓶颈**。

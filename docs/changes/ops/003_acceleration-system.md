# 003 提速方案系统化落地（2026-10-28，人定同意方案 1–7）

> 来源：`docs/changes/ops/003a_acceleration-review-20261028.md`（复盘与方案 1–7）；人定：2026-10-28 对话「新方案 1～7 都同意」。
> 本文回答「每条方案落到哪个文件、谁持有、怎么验收、怎么量效果」。

## 一、根因：方案落不住的结构性原因

1. **负控的事实标准不在库里**：`_tmp/20261025-claude-loop/negctl-rules.md`（派单时自动附给 worker）是唯一成文规则；`specs/` 无负控标准，CLAUDE.md 无一条负控祈使句。规则只活在协调者本机与 memory，评审与 worker 各凭理解。
2. **协调工具不在库里**：46 个派单 / 收货 / 评审 / 宿主链脚本在 `_tmp/20261025-claude-loop/`、`_tmp/20261026-macmini-plan/`，未入版本控制。后果：改动无评审（chain4 漏 chmod 致假通过）、靠复制成 chain3/4/5 与 phase-local-node22b 规避「不改运行中脚本」、换机或换会话即丢。
3. **效果无度量**：「提速有没有用」只能靠回忆拼时间线。

## 二、三层落点（沿用 CLAUDE.md 分工线）

| 层 | 放什么 | 位置 |
|---|---|---|
| 标准（陈述 / 判据） | 负控与判据的合格标准、正负例 | **新增** `specs/negative-controls/README.md`（由 `negctl-rules.md` 升格 + 本文新增条款） |
| 祈使（硬约束一句话 + 指针） | 「判据类单先红队」「超时不算杀」等 | `CLAUDE.md` 第五节加 2–3 行，只放指针 |
| 执行（代码 / 脚本） | 产品侧装置走 issue；协调工具入库 | 产品侧：`src/pilotdeck-gateway/tests/fixtures/…`；协调工具：**新增** `scripts/coord/`（从 `_tmp` 迁入，运行一律从 `_build/wt-tools-main` 已合入版本调用） |

> 「新顶层目录 = 协议事件」：本方案不新增顶层目录（`specs/negative-controls/`、`scripts/coord/` 均为既有顶层下的子目录）。

## 三、逐条落点

| # | 方案 | 落到哪些文件 | 类型 | 验收 / 度量 |
|---|---|---|---|---|
| 1 | 负控变异体预编译（mutant schemata） | 标准：`specs/negative-controls/README.md`「变异开关契约」节；代码：`src/pilotdeck-gateway/Cargo.toml` 加 `pdrs-mutants` feature（仅测试构建）、`tests/fixtures/_shared/mutant_switch.py`（按环境变量逐个开关运行）、试点 fixture 1 个（建议 `edgeclaw-service-facade`） | **产品单**（试点），两家评审 | 试点套件新旧机制 KILLED 集合逐条一致；单套耗时（旧：N 次编译 → 新：1 次编译 + N 次运行）写进 PR；Cargo 改动须过 manifest 守卫 |
| 2 | 共享编译缓存 sccache | `scripts/coord/cargo-env.sh`（导出 `RUSTC_WRAPPER`、缓存目录与上限）；macmini 与本机各 `cargo install sccache`（源码构建，不下预编译）；`scripts/coord/macmini-chain.sh` 引用 | 协调工具（最小修复，不开单） | 同一 HEAD 两次宿主链编译耗时对比；macmini 每棵 runner 树体积 |
| 3 | 主动扫时序偶发 | 新 issue：用 `stress-bin.sh` 高负载批跑 lib + 集成测试，产出「对固定时长敏感的用例清单」，逐条改为按进展 / 可控时钟；改动落在各 `src/**/tests*.rs`、`tests/*.rs` | **产品单**（测试） | 清单 + 每条改前 / 改后在负载 30+ 下各 25 次的通过率 |
| 4 | 负控登记改为脚本内标记 | `src/pilotdeck-gateway/tests/fixtures/anchor_selfcheck.py`（读脚本头 `# anchor-selfcheck: unsupported <理由>`，保留旧字典兼容一轮后删除）、`test_anchor_selfcheck.py`、现有 56 个 UNSUPPORTED 脚本头各加一行 | 自检装置，**挡住产品合入 → 最小修复单**，一家评审 | 合入后新增负控脚本不再改共享文件；`anchors-only` 0 errors |
| 5 | 判据先红队清单、结构化证据优先 | 标准：`specs/negative-controls/README.md`「判据设计」节（攻击清单模板、禁止从日志文本推断、超时一律 INVALID）；祈使：`CLAUDE.md` 一行指针；派单：`scripts/coord/task-templates/ruler-extra.md`（判据类单自动附加） | 规则 | 判据类单平均修复轮数（基线：#166 6、#775 7、#933 5、#61 4） |
| 6 | 收货 → 评审自动串联 | `scripts/coord/pipeline.py`：交付守望发现 exit=0 → `receive-patch` → `safe-push` → 按 diff 推负控 → `review-phase`；两家 0/0 时生成 PR 描述草稿（先人工确认再开 PR） | 协调工具 | 「交付落地 → 评审开跑」间隔（基线：数分钟到 4–5 h） |
| 7 | 宿主磁盘自愈 | 已落地：`_tmp/20261026-macmini-plan/remote-host-chain5.sh`（开跑前 < 35 GiB 自动回收 / 清空闲 target）→ 随迁移进 `scripts/coord/macmini-chain.sh` | 协调工具（已做） | 磁盘拒跑次数（基线：10-28 当天 3 次） |
| 0 | 度量 | `scripts/coord/timeline.py`：从各单 `_tmp/…/task.md`、`*.exit`、`phase.exit`、`gh pr` 时间生成「派单 → 交付 → 评审 0/0 → 合入」表，按日输出到 `_tmp/<日期>-timeline/` | 协调工具 | 每日一表；里程碑时蒸馏进 changelog |

## 四、协调工具入库规则（scripts/coord/）

- 只迁「在用且稳定」的：dispatch-issue / dispatch-fix / dispatch-resume、receive-patch、safe-push、neg-for-diff、review-phase（合并 review-phase2 与 phase-local-node22b，WHO 支持 codex / glm / dsh / glm+codex / glm+dsh，OVERLAP 选项）、macmini-lane、macmini-chain（chain5）、macmini-trim、recycle-tree、new-tree、watch-deliveries5、watch-queue2、enqueue-when-green2、human-comments、sigint-default、cargo-gate。其余 `_tmp` 旧版本留档不删。
- 版本靠提交链，不再复制 chainN / xxx-b；运行一律从 `_build/wt-tools-main`（已合入版本）调用，改动走 PR → 合入后 `wt-tools-main` 前进，天然避免「改运行中的脚本」。
- 不开 issue、不进 READY（CLAUDE.md「只做产品代码」）；PR 一家非作者评审（脚本冒烟原始输出贴 PR）。

## 五、执行顺序

1. 本文 + 决策卡入库（docs PR）。
2. 并行开两张产品单：方案 4（anchor 标记，挡合入，先做）、方案 3（时序偶发扫描）。
3. 方案 1 试点单：等方案 4 合入后开（同改 fixtures/_shared 附近，避免同模块并行）。
4. `scripts/coord/` 迁移 PR（含方案 2 cargo-env、方案 6 pipeline 到「起评审」、方案 0 timeline）。
5. `specs/negative-controls/README.md` 升格 + CLAUDE.md 指针（方案 5），随第 4 步或单独 docs PR。
6. 两天后用 timeline 表复盘一次效果，写进 board。

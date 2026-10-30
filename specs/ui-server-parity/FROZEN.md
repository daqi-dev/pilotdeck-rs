# B2 ui/server 尺子冻结登记

> **2026-10-18 重新冻结**（AI 代批，待人复核）。#94（PR claude/b2-94：94.1 审批白名单注释与空行规则、94.2 pnpm-workspace 手写子集解析去宿主 PyYAML、定标磁盘上限 CAP_BYTES 对齐人定 80 GiB、accepted-diffs 登记）改动装置后重新定标：HEAD ce77149，仓外 detached checkout，同一 UTC 日（2026-10-18）20 遍 TS 参考池 + 5 遍 TS 计分 + 1 遍 proxy；TS 十对 10/10、proxy PASS、26 遍时钟 PASS。**ref16 首采 REFUSED**（wall clock regression relative to monotonic clock），按 README 程序只重采该遍后 PASS，原档保留于批次 attempts/。两批依赖覆盖逐项相同（node_modules 1594、workspace_packages = [apps/desktop, packages/sdk, ui]、link_targets 1483、covered_roots 4），但 node_modules 摘要不同（10-18 `a619eaf9791e0820ff0fba15be6c14f97bee3d288c5b37772f4efd76670f9798` / 10-14 `b744de7b99a0b01925b0e7909c85f64bcf7c4a8a27186e4964d1dd7156b1c097`）：基线 ui/ 于 10-15 被写入 ui/dist（Vite 构建产物，约 200 文件）与空目录 ui/node_modules/.vite-temp，apps/desktop、node_modules、packages/sdk 三根自 10-14 起无改动；本批在新摘要下完整同日定标。两家冻结评审（GLM、DeepSeek）均「同意」、无 P0/P1；DeepSeek 首轮因附件不足判「不同意」，经协调者补核后改判同意（两轮原文均存 approval/）。上一版冻结（10-14，批次 d3879d24）随装置变更失效，原件移至 approval/2026-10-14/。
> **2026-10-26 重登记（#209 B2 共享锁释放门控，AI 代批，待人复核）**：`scripts/ui-server-parity/calibrate.sh` 去掉无条件 `trap 'rmdir /tmp/pdrs-parity.lock' EXIT`，把租约与信号清理内联进 `calibrate.sh`：获取时先发布 `cleanupPending: true`，`calibrate.py` 在独立 POSIX 会话中启动并登记 pgid；SIGINT/SIGTERM 先 killpg 本次会话组，经有界 `TERM→wait 1s→KILL→wait 1s→wait 2s→killpg(0)` 证实 ESRCH 后才删除租约；任一步 kill/wait/探测失败都把残留 pgid 原子写进 `owner.json` 的 `cleanupPending` 并以 BLOCKED(2) 退出，不自动接管。只改外壳的锁/信号生命周期，`calibrate.py`、比较器、阈值、分母、归一化、场景与定标输入均未改；`test_calibrate.py` 原字节未动。`src/pilotdeck-gateway/tests/fixtures/parity-lock-release/test_lock_release.py` 直接抽取并执行同一段内联监管器，覆盖信号挂起、正常退出保留子进程码、中断后回收并释放、残留组保留、cleanupPending 拒绝接管和 shell 无无条件 trap。本机找不到 2026-10-18 定标原始批次（`_tmp/archive/b2-calibration-2026-10-18-a025aa97` 不在当前主机），故未重跑 20+5+1 定标；对照方式：在同一 UTC 日按 `scripts/ui-server-parity/README.md` 用 `bash scripts/ui-server-parity/calibrate.sh` 新建批次，TS 十对与 proxy 结论应与 approval/calibration-summary.json 的 PASS 一致。当前可复算的结构证据是 `python3 -I scripts/ui-server-parity/check_frozen.py` 与 `python3 -I scripts/ui-server-parity/test_calibrate.py`（原始输出见 `_tmp/20261026-issue-209/raw/`）。

> **2026-10-29 #1023 重登记**（AI 代批，待人复核）。仅同步 #701 的隔离旁路，不改判分/比较语义：离线测试命令从 `python3 -I -B -m unittest discover` 改为逐文件 `python3 -I <test 文件>`（禁止 discover；两个测试文件补 `unittest.main()`），22 个 Python 入口的守卫增加「装置目录（含 `controls/`）不在 `sys.path`」。对 10-18 存档 `_tmp/archive/b2-calibration-2026-10-18-a025aa97` 做 historical-replay 复算（证据 `_tmp/20261026-issue-1023/replay/`），十对 TS 与 controls 结果不变；历史 replay 不能定标，本装置仍需下一次真实 k=5 定标。同批复核 `#1023-1`…`#1023-4` 负控。**2026-10-30 fix-r3**：22 个 Python 入口守卫改身份比较（`os.path.samefile` 设备/inode；大小写别名、symlink 及混写 `//` 指向同一目录即拒绝，不存在的 `sys.path` 项跳过），不再用 realpath 字符串相等；判分/比较语义不变。新增 `#1023-5` 身份锚点与 `#1023-6` 别名运行时回归；负控在 private mirror 把守卫换回 realpath 变体，绑定 `#1023-6` 专属断言。
> **2026-10-30 fix-r4**（#1023，AI 代批，待人复核）：22 个入口守卫在 `import os` 前增加
> fail-closed 预检（`sys.version_info < (3, 11)` 或 `'os' not in sys.modules` 即打印既有
> FAIL JSON 退出），因为 `os` 自 CPython 3.11 冻结且 `-I` 启动已载入才不可被重插入目录
> 的同名 `os.py` 顶替；旧解释器或 `-X frozen_modules=off` 且未预载时伪造 `os.py` 会先于
> 守卫执行，属尺子 P0 级旁路。新增 `#1023-11` 运行时回归（三种场景 × 22 入口，伪造模块
> 用标记文件证明未执行）与 `os-preflight-removed` 负控。判分/比较语义未变；本轮仅重登记
> 上述 22 行哈希。
> **2026-10-30 fix-r5**（#1023，AI 代批，待人复核）：`test_frozen.py` 的 `#1023-11`
> 回归由空转夹具改为真实注入（`-X frozen_modules=off`、卸载 `sys.modules['os']`、
> 真实 forge 目录上 `sys.path`、独立传标记文件），并加配对控制：删掉预检的同一入口
> 必须执行伪造 `os.py`（标记文件出现），正式入口必须精确命中预检且无标记。另新增
> 6 个 fixture 守卫的 os 预检与装置目录重插入回归。22 个入口、判分/比较语义与阈值
> 未变；仅重登记 `test_frozen.py` 一行。
> **2026-10-14 重新冻结**（AI 代批，待人复核）。PR #93（#59 路由审批门与白名单、#80 反代 WS 四项、#88/#89 P2）改动装置后重新定标：main eb2f2c7，仓外 detached checkout，同一 UTC 日 20 遍 TS 参考池 + 5 遍 TS 计分 + 1 遍 proxy；TS 十对 10/10、proxy PASS（7 个 WS 握手无 Date、下行 close 各一）、26 遍时钟 PASS、无重采 / REFUSED / 近似测量。两家评审（GLM、Claude）均「无 P0/P1，可冻结」。上一版冻结（PR #90，批次 7d28f0de）随装置变更失效。Rust 路由豁免审批须为 human-approvers.txt 白名单身份（与本冻结记录的代批规则不同，见 acceptance.md）。
> 定标命令及重采规则：`scripts/ui-server-parity/README.md`；不得借历史 replay 定标。
> 冻结时由协调者补定标证据与两家评审路径、替换下方审批及依据占位，再移除首行「待定标」。`Approved-By:` 接受具名人或「AI 代批，待人复核（协调者）」，并必须有 `Approval-Basis:` 记明定标报告路径和两家评审路径。冻结后变更需遵循 CLAUDE.md §五。
> 校验：`python3 -I scripts/ui-server-parity/check_frozen.py`；格式沿用 gateway：「sha256 两空格 相对本目录路径」。草稿哈希全一致退出 2 / DRAFT，缺项/漂移/未登记装置文件退出 1 / FAIL。
> 清单覆盖规约四份及 rust-routes.json、human-approvers.txt（路由审批白名单；改动属装置变更，须评审）、scripts/ui-server-parity 与 scripts/exp008 两目录的全部 Git 跟踪文件；实际文件必须等于 Git 跟踪清单，额外/未跟踪/缺失文件、链接、__pycache__、*.pyc 一律 FAIL；仅精确名为 .DS_Store 的普通文件不登记、不执行、不参与 hash。根 rust-toolchain(.toml) 若存在也纳入清单。所有 Python 入口带 -I，装置预检源码自身先校验哈希，本目录模块按已验哈希的绝对路径载入。威胁模型与每批新构建规则见 README 和 acceptance.md；不防同时篡改装置、清单与审批的本机恶意者。

Approved-By: AI 代批，待人复核（协调者）
Approval-Basis: specs/ui-server-parity/approval/calibration-summary.json; specs/ui-server-parity/approval/review-glm.md; specs/ui-server-parity/approval/review-deepseek.md
Calibration-Evidence: specs/ui-server-parity/approval/calibration-summary.json; specs/ui-server-parity/approval/driver.log; _tmp/archive/b2-calibration-2026-10-18-a025aa97/final-ruler-333fbde2-b5d9-4b01-b29b-c2952ac87357.json; _tmp/archive/b2-calibration-2026-10-18-a025aa97/ts-ruler-55036e36-ee90-4304-a5ca-60ef5aeead02.json
Independent-Reviews: specs/ui-server-parity/approval/review-glm.md; specs/ui-server-parity/approval/review-deepseek.md

f6d385e8da46ed612ba5207dcdf588c8331cfe025a866bdb59bb7e02ff32a7cc  ../../scripts/exp008/compare.py
3b8bec2dbcd28b3e99ee11017e4e37c3e59d873d2800331decf0f90a1fd25b24  ../../scripts/exp008/controls.py
803783f412eaff275249d0af1750d054b637f83dc8c4137ece5384241e099e02  ../../scripts/exp008/driver.mjs
3ea3e6f27cb55c194ccf2a456e3fe5241042726f4b0a369cc13597ee108bcaab  ../../scripts/exp008/enumerate_hook.mjs
7d4c383a249c29f371c2312671458bf996c1f2f36669ba924dcd96bfea2ef63b  ../../scripts/exp008/preload.mjs
3ef27b94af7d6bdfc8b90c4f0ccead09f0f97422d3fa9f3d7dab3e4e146fe605  ../../scripts/exp008/proxy/Cargo.lock
56a68e9239f0701c46a6b42ef3316b2ac3ff9b238bed2b086a4f8f297d64609f  ../../scripts/exp008/proxy/Cargo.toml
abde68d203a8c76330668554b3606a10a94c1e4a145effcb3cbfc856556aadce  ../../scripts/exp008/proxy/src/main.rs
89e62305ff30e49caefd8f649657b6aa1a9c968700e37509a705330ec21d8224  ../../scripts/exp008/proxy/tests/node-ws-semantics.cjs
ed5d447d3ac2e4ba03def27645f355997be981e3d82012427f14b1f1a76870c1  ../../scripts/exp008/reconcile_routes.py
a60f04b6ce5cccab4c40c75b0a6d8204344977bde85d4931dde213b18ddd126e  ../../scripts/exp008/run.py
980130c241a926b0854c719210b8d1ccdbcf294f99bde3beb567a5bfda072d90  ../../scripts/exp008/vite.config.mjs
c8a21ce64dfa798a64667f82777704175b520115263f1917f56e0010c4b1754b  ../../scripts/ui-server-parity/README.md
7bab891b7690b21c2442229e7406d8d0d9e4feb2352df267852f61ac9acd9cba  ../../scripts/ui-server-parity/apparatus.py
2586ba1c070e3e5a168820807051ca1b1845df091f6fae48f0dc6935d9fae180  ../../scripts/ui-server-parity/benchmark_clock.py
209743e7db9181f7faa6a645c4859bb71be6b784b40c208fad001dec1babfcf3  ../../scripts/ui-server-parity/calibrate.py
e8724af2dd0c7536dc514c136ed8b4ae099257aeae461ff8b303fd22111c8f7a  ../../scripts/ui-server-parity/calibrate.sh
206000021604a6f45890b1a3f8bc919117843452ca08648267de9c851b03ecd6  ../../scripts/ui-server-parity/chat_driver.mjs
1c25f6b3013f28bfffea29becc0c52a4e452749a1487b6e6fbe8b988ad687879  ../../scripts/ui-server-parity/check_coverage.py
28d86c7395d5923e436a49d0a11339bd8c49325881ff21dbcbb72842b36c5dd0  ../../scripts/ui-server-parity/check_frozen.py
2da03a3ce2f10676faec30dec9bb9bb51e57f2830ba22417f8ff3e8b476c54f8  ../../scripts/ui-server-parity/check_ruler.py
621c35912de50a7c4496a8db589e28d09cbb18ecc42714eca9cb03ad6cc87ba9  ../../scripts/ui-server-parity/common.py
d67728d6c560ccacfcc87da7a10bc7f68a0369d98eac63e5d9c2f85760e0ed83  ../../scripts/ui-server-parity/compare.py
f785d81c732e18a4607eae11a845a1e7015db831150f0fc89a72f0580ebe89f9  ../../scripts/ui-server-parity/controls/README.md
9aebd4dd53fbc90b541b0fa34b5b326d751f0548431a0b3385a45082702b9d99  ../../scripts/ui-server-parity/controls/identity_paths.py
acb5b61e5cc1742715a32cf12c3688d40d547b12950e9e32059bcb98e2cf8dc2  ../../scripts/ui-server-parity/controls/multimodal.py
52a1a42136d6bfe21758b5c6cc0d00c7794153ac69cbdc975339ccd1d6370ed2  ../../scripts/ui-server-parity/controls/run_all.py
ff9a48425b1bb2715eee497ab4f9040780b0be532603d69061acb9776b262bfc  ../../scripts/ui-server-parity/controls/streams_and_clocks.py
acdc238631ed73d7558d6a54ff44f3a837c3466af723e5cfc6ede78b2a5c0cb6  ../../scripts/ui-server-parity/coverage.json
7665bb29aedeced0900e68011413ca447d28a55d512eeda6122ed49b4ffdc1f8  ../../scripts/ui-server-parity/driver.mjs
9d36c543ee29884fb5c578c1d1fd14b24d8394626b5e7deb33f8f149ef22af26  ../../scripts/ui-server-parity/enumerate_routes.mjs
79f2e369a60fbf7616c52ea8408a16d23330108b43886a147a911e818633fde1  ../../scripts/ui-server-parity/multimodal.py
6f820bf09d30dc34af36d9a936c6c124488a08097c6c706596d684c0e0de5c4d  ../../scripts/ui-server-parity/policy.py
b45b2fff9623f7647adb14d9e1baa73c48da25a10fdbbba52f599313f728f06a  ../../scripts/ui-server-parity/recording.rs
b8f30d6fd2228734b17569722362c51cc20a7622b000b876c27a899218a0364e  ../../scripts/ui-server-parity/run.py
bece605c49006c2f13f5de1a5550dc82616c5f469a0d172b164a765f5457cbd6  ../../scripts/ui-server-parity/scenarios.json
cc6e3a2be2497e900bd55e6fbce0d8c874c663288502cbdcbd8ff8b8bb919cb1  ../../scripts/ui-server-parity/snapshot.py
54138118013854a3ad42b51a67cf6ea911b67858e7cbdd719d9c57128be6cac8  ../../scripts/ui-server-parity/summarize_ruler.py
2606ae90fb8e155c6a7989aca64a2944d9fc12e170be18fc73a7846da64b0945  ../../scripts/ui-server-parity/test_calibrate.py
fce6b194ea7ac70698fcc957e5064a2f51633852ed6adeb298d65529f0ac2976  ../../scripts/ui-server-parity/test_chat_driver.mjs
d796d56ee40603bd9f9c0ef9c735eb118867b722ec57cc0e57b8cf1245bd36b6  ../../scripts/ui-server-parity/test_compare.py
17b60d89802f87ec968354ed5d34969498a906b264fcff3dab9781e8215341c7  ../../scripts/ui-server-parity/test_coverage.py
0d19757e5cca65a9217a9308e548e2ff730e63e14bcb4b7712c53fd687193b45  ../../scripts/ui-server-parity/test_frozen.py
43a7089678997c03dea57219faf29a67b8325c98078ac4696e788b760bb98a5b  ../../scripts/ui-server-parity/test_multimodal.py
6bcc00d4e369a0374d8366110f849ed79f212e0f0bdfe9848372953150418f40  ../../scripts/ui-server-parity/test_recording.py
9ccf2a7fef528fdfecea33b8c9dc0c9cbd2982f09631b6fe2f3e377dca122d5e  ../../scripts/ui-server-parity/verify_auth.mjs
1dd44329badbd0b0676a4176b4adea998cc03ccfd9ec7ad4664d3e0899f780c3  ../../specs/ui-server-parity/acceptance.md
ea5ea81bbfc77c78ad607bc9a29864bad866d8eb5a9dc9bb246f64b0cb5cd32c  ../../specs/ui-server-parity/accepted-diffs.md
b2004ddf95e02c24b307a9c20905d76437c3eae1a251cff189dd6836e3be85bf  ../../specs/ui-server-parity/multimodal.md
37cd7d5f8358cdac36cf0d2bd5d5c09a73fa0d429d5225583d61abe261441b02  ../../specs/ui-server-parity/normalization.md
a1cf65d93b72351eaf5fce8c5356bfaa3b40f56f401d41151123b78582412fc4  ../../rust-toolchain.toml
7670c4a531150c7886d224ecdebb9eca3fa7724ac91ac8632cc98446d501964f  ../../specs/ui-server-parity/rust-routes.json
0365b045f3ec15853726a26ccd7b4b6f54993c3d52d6c667f9b6e81341121a65  ../../specs/ui-server-parity/human-approvers.txt

## 批准依据可移植化（#113，2026-10-16）

仅迁移原定标摘要和两份原评审的引用，不更改已登记53文件、比较器、输入或判分。原始摘要保留PENDING_REVIEW/null/frozen=false的历史现场；最终批准由本登记及两份原评审共同记录。来源与残余边界见[批准依据索引](approval/README.md)。

## 2026-10-18 重新冻结的批准依据（#94）

当前四份原件：定标摘要、两家冻结评审、采集驱动日志（三段：首跑至 ref16 REFUSED、ref16 重采、续跑完成）。厚 ruler 留归档，仓内只登记路径与哈希（见 approval/README.md）。10-14 的四份原件移至 approval/2026-10-14/，历史保留、不再作为当前依据。

Approval-Source-SHA256: 22aebcd1ed849525a2c23b43b0e88cc09cfa9c6a72c5a5871d3377da9b370bfc  specs/ui-server-parity/approval/calibration-summary.json
Approval-Source-SHA256: 121cf870e657293c7a4e185742ac57b3941f55f93c7057fb267f77a07932177f  specs/ui-server-parity/approval/review-glm.md
Approval-Source-SHA256: d46e9ab090abec3c505c8a49da1a33f895e6d7e7eaf73153905408777dc464ec  specs/ui-server-parity/approval/review-deepseek.md
Approval-Source-SHA256: 5df5ae8b591ea4daa254ca8ae95e7910a68d789145eb8e94ee7e6d0a857eed6d  specs/ui-server-parity/approval/driver.log
Approval-Archive-SHA256: 5fe7f6c6dde4f2f1082cc0c986839397b2e6b42e05156a9008ffcba8c51e9c18  specs/ui-server-parity/approval/2026-10-14/calibration-summary.json
Approval-Archive-SHA256: 108245ecb62996a9c88aa93659cfbcca6cc9e31c378dbc6983cd07da3ce3cf99  specs/ui-server-parity/approval/2026-10-14/review-glm.md
Approval-Archive-SHA256: 5d3e308c3ab508f542b1eaf095465e240433ac13c62942f8b7680fcb93d7f890  specs/ui-server-parity/approval/2026-10-14/review-claude.md
Approval-Archive-SHA256: 4efbe795b6f06d53fdc92951b40a342c1c3c55ab4607d59f848666351a8ed730  specs/ui-server-parity/approval/2026-10-14/driver.log
Approval-Archive-SHA256: 8fcf35c26663ed60cdf7bda58fd026ba66f87fa7f4b9c3332848d8d4dc1af2db  specs/ui-server-parity/approval/review-deepseek-r1.md

## 2026-10-30 #1088 账本一条一文件重登记

AI 代批，待人复核。依据 #1085 人定 D / #1088 PR-1，只移植 gateway 的 diffs/ 六字段校验、组合阅读与 --base 只新增保护；不跨装置 import。历史 accepted-diffs.md 原字节保留，新条目与更正分别新增文件；不改比较器、阈值、分母、场景、定标输入和判分路径。

仅重登记本次 check_frozen.py 哈希。旧新真实登记逐项对照、负控、两家非作者评审及待验项目见本树 `_tmp/20261026-issue-1088/RESULT.md`；既有定标和历史批准依据不改。本次重登记的最终冻结仍须两家非作者评审完成及人复核，不以旧批准记录代替本次评审。

本次账本存放规则也取代现有 acceptance.md / README.md 中“追加到 accepted-diffs.md”的旧指令；这些冻结规约的字节与判分语义保持不变。完整账本的阅读与更正规则以 diffs/README.md 为准。

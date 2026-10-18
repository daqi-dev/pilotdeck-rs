# B2 ui/server 尺子冻结登记

> **2026-10-18 重新冻结**（AI 代批，待人复核）。#94（PR claude/b2-94：94.1 审批白名单注释与空行规则、94.2 pnpm-workspace 手写子集解析去宿主 PyYAML、定标磁盘上限 CAP_BYTES 对齐人定 80 GiB、accepted-diffs 登记）改动装置后重新定标：HEAD ce77149，仓外 detached checkout，同一 UTC 日（2026-10-18）20 遍 TS 参考池 + 5 遍 TS 计分 + 1 遍 proxy；TS 十对 10/10、proxy PASS、26 遍时钟 PASS。**ref16 首采 REFUSED**（wall clock regression relative to monotonic clock），按 README 程序只重采该遍后 PASS，原档保留于批次 attempts/。两批依赖覆盖逐项相同（node_modules 1594、workspace_packages = [apps/desktop, packages/sdk, ui]、link_targets 1483、covered_roots 4），但 node_modules 摘要不同（10-18 `a619eaf9791e0820ff0fba15be6c14f97bee3d288c5b37772f4efd76670f9798` / 10-14 `b744de7b99a0b01925b0e7909c85f64bcf7c4a8a27186e4964d1dd7156b1c097`）：基线 ui/ 于 10-15 被写入 ui/dist（Vite 构建产物，约 200 文件）与空目录 ui/node_modules/.vite-temp，apps/desktop、node_modules、packages/sdk 三根自 10-14 起无改动；本批在新摘要下完整同日定标。两家冻结评审（GLM、DeepSeek）均「同意」、无 P0/P1；DeepSeek 首轮因附件不足判「不同意」，经协调者补核后改判同意（两轮原文均存 approval/）。上一版冻结（10-14，批次 d3879d24）随装置变更失效，原件移至 approval/2026-10-14/。
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
d09aac5d72d1fcd87e6b9f51927fcb859ddf6653d52f1b24295dd6a7c0c2b83c  ../../scripts/ui-server-parity/README.md
7bab891b7690b21c2442229e7406d8d0d9e4feb2352df267852f61ac9acd9cba  ../../scripts/ui-server-parity/apparatus.py
985515952fd382a08c9766c6bd507baf0192a8499a975d4479f62fcb7dc39711  ../../scripts/ui-server-parity/benchmark_clock.py
9163d4689eb3f79ce0d6b10ce754158fcabd11a067ab513f8c67453bcb78c8bc  ../../scripts/ui-server-parity/calibrate.py
58f1641047497c8076ea93ed2119ffd7aa62601ff2360a99b79f1e55a2a91194  ../../scripts/ui-server-parity/calibrate.sh
206000021604a6f45890b1a3f8bc919117843452ca08648267de9c851b03ecd6  ../../scripts/ui-server-parity/chat_driver.mjs
88b788b631e956f9b4fd3219bbfd53a5e5fd52215c1d51233da9cb3fc0442200  ../../scripts/ui-server-parity/check_coverage.py
517ef5940a65abcff8c193ba77b3bff11b92750ac7d0db86e63f3043e91bd48a  ../../scripts/ui-server-parity/check_frozen.py
83a0026fd43d885e857015b826242fbe2ec68706e419e1ba150d8157e44a2125  ../../scripts/ui-server-parity/check_ruler.py
aaad3a98e33f0e333c1ae9e06f74fdf0f43e23c2337a4fb1098b6ae0d5fc6839  ../../scripts/ui-server-parity/common.py
e09b8199597d3a4b9751a056b168d7e9272a5c3e1d18053e56590c10642e8ea7  ../../scripts/ui-server-parity/compare.py
f785d81c732e18a4607eae11a845a1e7015db831150f0fc89a72f0580ebe89f9  ../../scripts/ui-server-parity/controls/README.md
0399a28bae1fed9a52593d981959de8380996f5ebe4b3f4854bde5622a0506da  ../../scripts/ui-server-parity/controls/identity_paths.py
4b5c5be5d20dff8f3309d52baf6849256db48006c6e5c2817acf4857004a3376  ../../scripts/ui-server-parity/controls/multimodal.py
a1de16ba7dc08e239ec1f4fb08ccf4e327ac5e0efd48e0df4d967ded5f7a1b74  ../../scripts/ui-server-parity/controls/run_all.py
931bb0975fc6341c97889514d8901dcf666160deb061e3a3bdde31dc7f7c8b3f  ../../scripts/ui-server-parity/controls/streams_and_clocks.py
acdc238631ed73d7558d6a54ff44f3a837c3466af723e5cfc6ede78b2a5c0cb6  ../../scripts/ui-server-parity/coverage.json
7665bb29aedeced0900e68011413ca447d28a55d512eeda6122ed49b4ffdc1f8  ../../scripts/ui-server-parity/driver.mjs
9d36c543ee29884fb5c578c1d1fd14b24d8394626b5e7deb33f8f149ef22af26  ../../scripts/ui-server-parity/enumerate_routes.mjs
d86ed3727dcc4b50902add4a1cd49da4e93305776eceaacc8049247ac2d1330b  ../../scripts/ui-server-parity/multimodal.py
83cbac908c33a58b46d015965950fb020d6489b7c3d76102f7f40242e42f09da  ../../scripts/ui-server-parity/policy.py
b45b2fff9623f7647adb14d9e1baa73c48da25a10fdbbba52f599313f728f06a  ../../scripts/ui-server-parity/recording.rs
959951479084cb739b5136f723575cdbc1f5a01f6ec12536c9c8c9428a0dfe65  ../../scripts/ui-server-parity/run.py
bece605c49006c2f13f5de1a5550dc82616c5f469a0d172b164a765f5457cbd6  ../../scripts/ui-server-parity/scenarios.json
f74dd7c3acfb8a3c44c750772f98b9af93662539ce65dbe9aea7297ce638d389  ../../scripts/ui-server-parity/snapshot.py
03759b276dd861191b7372fb85ede5277677a6bc5db12901cb64e0d81d9f54ce  ../../scripts/ui-server-parity/summarize_ruler.py
464b7b0e9497e1893a87793fb980232b65dd66da17877365c8c519c0f6c18f25  ../../scripts/ui-server-parity/test_calibrate.py
fce6b194ea7ac70698fcc957e5064a2f51633852ed6adeb298d65529f0ac2976  ../../scripts/ui-server-parity/test_chat_driver.mjs
bdd5bc5f85ac703a8fca8b4221f5c06f56efd94c02c47dd118854485e02f048b  ../../scripts/ui-server-parity/test_compare.py
cf77c2209bc64b566f4ed6ebc739cc34cdbe13a379475fd9519edb9d1eb2e73b  ../../scripts/ui-server-parity/test_coverage.py
ac4cd82e1076e8e19ef35b6e65e93bf090eb3961b1022b9e50a672b6cd91a1a6  ../../scripts/ui-server-parity/test_frozen.py
75d9418fa858c51ca9cd1bae960137be53edfe94d3e2b4f41413a6f4741a29ac  ../../scripts/ui-server-parity/test_multimodal.py
8568dbaffd2b20c070a20a773ab4fe73110d01def63f6364f6a007985c0fcca9  ../../scripts/ui-server-parity/test_recording.py
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

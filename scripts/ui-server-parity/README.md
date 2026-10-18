# B2 装置与待定标采集

规约正本为 `specs/ui-server-parity/{acceptance,normalization,accepted-diffs,multimodal}.md`，均为 DRAFT。`FROZEN.md` 首行保留「待定标」，哈希检查通过也只能 DRAFT/2；去掉占位、登记审批与依据之前不能宣称冻结。`Approved-By:` 接受具名人，或精确的「AI 代批，待人复核（协调者）」；两种方式都必须同时有唯一的 `Approval-Basis:`，写明定标报告路径及两家独立评审路径（至少三个不同的具体文件路径）。空值、占位与重复字段不能批准。采集装置 hash 绑定四份规约、全部装置及 B1 共用依赖；审批清单 FROZEN.md 自身不参与 capture hash，避免补定标/评审/审批字段后反过来使本批采集失效。

威胁模型（协调者自决，**AI 代批，待人复核**）：防意外漂移，以及 worker / 开发者在仓库内对装置的改动，包括未跟踪文件、影子模块和旧构建产物。不防拥有本机写权限、能够同时篡改装置、哈希清单与审批的恶意者；模型外发现按 P2 跟进。Git 跟踪清单、FROZEN 哈希和审批材料是本模型的信任依据。

装置目录 `scripts/ui-server-parity/` 与 `scripts/exp008/` 的实际文件必须**恰好等于 Git 跟踪内容**，两目录的全部跟踪文件都进入 FROZEN 和 capture hash。精确名为 `.DS_Store` 的普通文件不执行、不登记、不参与 hash；同名链接、目录或特殊文件仍 FAIL。除此以外任何多余/未跟踪文件（含被 .gitignore 忽略的文件）、缺失跟踪文件、符号链接（目录/文件/断链）、`.pyc`、`__pycache__` 或非普通文件均 FAIL。递归扫描不跟随链接，不以排除缓存或影子模块的方式继续运行。审批清单 FROZEN.md 自身不进入 capture hash。

所有 B2 Python 入口和 Python 子进程使用 `python3 -I`，隐含 `-E -s`；入口先断言 isolated、ignore_environment、no_user_site 和 `sys.pycache_prefix is None`，不带 `-I` 或显式 `-X pycache_prefix` 均 FAIL。`PYTHONPATH`、`PYTHONHOME`、`PYTHONUSERBASE`、`PYTHONPYCACHEPREFIX` 等环境变量由解释器忽略。入口在执行装置预检源码前先校验其 FROZEN 哈希；清单缺失时只允许与 HEAD 相同的装置源码，checker 仍返回 DRAFT/2。本目录模块由显式绝对路径的源码加载器校验哈希后执行，不走 pyc，也不把装置目录加入 sys.path。B1 compare/run 同样先验哈希，加载结束恢复 sys.path 和 compare 模块绑定；reconcile 子进程也带 `-I`。关闭字节码写入；unittest 的第一个测试由标准库 discovery 导入，因此测试命令额外带 `-B`，避免在入口代码执行前写缓存（`-I` 会忽略 PYTHONDONTWRITEBYTECODE 环境变量）。

正式定标开始、每遍前后及续跑均要求装置路径 `git status --porcelain --untracked-files=all` 为空（含四份规约及 FROZEN），并在 batch.json 记录 HEAD；HEAD 与批次记录不同即拒绝，必须另建批次。根目录 `rust-toolchain` / `rust-toolchain.toml` 若存在必须为 Git 跟踪的普通文件，纳入 FROZEN、capture hash 和 Git admission。每批独占全新 `batch/cargo-target/` 与 `batch/cargo-home/`，已存在的 target 一律拒绝；Cargo 使用该目录构建，记录 target 路径、产物路径与 SHA256。每遍 run 只使用本批校验通过的产物，续跑不重建、不复用 `_build` 的旧产物；独立 run 也在其新输出目录内构建新 target。

`Approved-By:` 拒绝 TODO、模型名（Claude/GPT/GLM 等）和零宽/不可见字符；精确的协调者代批串仍可用。`Approval-Basis:` 至少三项不同的真实文件，按 realpath 去重，路径必须落在当前仓库或共享主仓的 `_tmp/`，不得借别名、不存在文件或符号链接逃逸凑数。

离线验证（不监听）：

```sh
python3 -I -B -m unittest discover -s scripts/ui-server-parity -p 'test_*.py'
python3 -I scripts/ui-server-parity/controls/run_all.py --out _tmp/b2-controls.json
python3 -I scripts/ui-server-parity/check_frozen.py --self-test
python3 -I scripts/ui-server-parity/check_frozen.py  # 草稿哈希正确时退出 2
python3 -I scripts/ui-server-parity/benchmark_clock.py
bash -n scripts/ui-server-parity/calibrate.sh
bash scripts/ui-server-parity/calibrate.sh --preflight
```

`calibrate.sh` 与 `calibrate.py` 入库并登记 hash，便于固定装置和复现；`_tmp/` 仅保存输出。正式采集须在**允许 loopback listen 的环境**运行，使用 Node 22 和已经备好依赖的只读 TS 基线，不触碰用户运行的 PilotDeck 或用户 home。

```sh
# 在当前 checkout 根执行；可显式传 --baseline 和 --size-reference。
bash scripts/ui-server-parity/calibrate.sh
# 输出 batch 路径；按 batch.json 的同一 UTC 日/同源绑定续跑。
bash scripts/ui-server-parity/calibrate.sh --batch /absolute/path/to/batch
# 某遍 REFUSED 时只重采该遍；例如 ref7。然后用上一条命令继续。
bash scripts/ui-server-parity/calibrate.sh --batch /absolute/path/to/batch --retry ref7
```

脚本持有 `/tmp/pdrs-parity.lock`，依次采 20 ref、5 TS，再先执行 TS-only `check_ruler`，通过后才采 1 proxy 并执行带 `--proxy` 的 `check_ruler`。每遍调用生产 `run.py` 的 ClockMonitor，保存并重算 `clock_regression_check`。所有运行使用该 batch 的固定 `slot/direct` 路径，服务停止后才改名归档；不改 capture metadata。结果为 `summary.json`，机器门全过的目标是 `PENDING_REVIEW`、TS 10/10、proxy PASS、26 遍时钟 PASS，独立评审仍 null。

REFUSED 原档和日志保留在 `attempts/`；重采只能替换那一个尚未接受的轮次。PASS 轮次不可覆盖；计分开始后不可替换参考池，池与计分身份/录制 hash 不重复、区间不重叠。跨 UTC 日、源码/装置变更、已有 slot 尚未归档、采集 FAIL、池分类 REFUSED 等需查看证据，不能用改 metadata 或 replay 标记补救。跨日/变更必须新建完整 batch。因中断留下 slot 时脚本拒绝覆盖，先按 audit 核实自有进程清理再人工归档。

磁盘按 git common dir 找到共享主仓，统计其 `_build` + `_tmp`（包括 worktree 内输出），上限保守取十进制 30 GB。默认只读查找历史 live3 单遍目录，取实际占用最大的完整遍作尺寸参考；也可显式 `--size-reference` 指定。预计每遍尺寸为历史尺寸 ×1.5，另预留构建 1 GB、报告 1 GB；开始、构建后、每遍前后和尺子检查前后重查，并校验文件系统可用空间。估算是启动门，若输出增长耗尽预算则停止并保留证据。历史 live3 只作磁盘估算，不进入新 TS 参考池。
`du` 非零退出仅在 stderr 每行均为 macOS 实测的 `du: <路径>: No such file or directory` 时按 1 秒间隔最多重试 3 次（共 4 次测量），仍失败则采用最后一次 stdout 合计并向 stderr 记录 `approximate: true`；权限等其他或混合错误仍 FAIL，预算上限与估算方式不变。

构建输入边界：只从仓内 `_build/cargo-home/registry/{index,cache}` 复制索引和压缩 `.crate`，不复制 `registry/src`、`git/checkouts`、config 或 credentials；装置在每次 fetch/build 前按 Cargo.lock 显式校验缓存 .crate 的 SHA256（Cargo 离线可能直接信任缓存），再由 Cargo 重新解压。fetch/build 都带 `--locked --offline`，缓存不足即 FAIL，须在开始新批次前备好压缩包。Cargo 的 cwd 为批次目录（独立 run 为 out），manifest 用绝对 `--manifest-path`。每次 cargo/rustc 启动前检查 cwd 与新 CARGO_HOME 的全部祖先，不得出现 `.cargo/config` 或 `.cargo/config.toml`（含断链）；新 CARGO_HOME 本身的 config/credentials 也拒绝。继承环境全部丢弃，只设置 HOME、受限 PATH、新 CARGO_HOME/CARGO_TARGET_DIR、宿主 RUSTUP_HOME 和禁止自动下载的 RUSTUP_AUTO_INSTALL。PATH 只含宿主 `~/.cargo/bin` 与系统目录，因此仓内 wrapper、RUST*/CARGO_*、linker 和动态加载变量不生效。构建前记录 `rustc -vV` / `cargo -V` 到 batch.json（独立 run 到 toolchain.json），每次 guard 复核同一工具链版本。

批次在构建前记录基线整个 `node_modules` 的单一 SHA256 与实测耗时到 batch.json.node_modules；相对路径、lstat 文件类型、普通文件字节 SHA256、空目录以及符号链接目标均参与摘要，链接先枚举目标闭包，不沿链接递归哈希，特殊文件拒绝。每遍 source_sha256/source_sha256_end 增加 node_modules，批次每遍前后、结束与续跑均重新计算并匹配最初摘要，漂移即 FAIL。旧批次/新采集元数据缺此字段不能用于定标；历史严格复算仍按 historical-replay 路径使用旧录制，不能取得 calibration_eligible。

第 5 轮依赖边界：递归枚举基线内任意深度的 `node_modules`（目录链接也登记，但不沿链接遍历）。pnpm-workspace.yaml 存在时读取其 packages（pnpm 优先；#94.2 起用 common.py 手写子集解析，不依赖宿主 PyYAML：只认唯一的顶层普通键、一层同缩进的单行 `- 值` 或 `键: 值`、注释与空行，packages 只收引号串或字母开头的普通路径词或行内 `[]`；flow 集合、锚点/别名/标签、块标量、多行或带转义的引号、嵌套、重复键、tab/控制字符/BOM 一律 FAIL，接受的输入与 PyYAML safe_load 结果一致，基线原文对照见 test_calibrate.py）；否则读取根 `package.json` 的 `workspaces`（数组或 packages 数组，支持 glob 和 `!` 排除）声明的普通包目录，以及实际符号链接解析到的基线内或仓外的目录或文件目标（包括无 package.json 的目标），完整树共同进入同一个依赖摘要，包含 SDK 的 dist。每条链接同时登记原始目标与解析后的目标；普通文件、空目录、文件类型和相对路径继续绑定。先枚举，再对覆盖树并集读取一次，嵌套根不重复读文件；没有 mtime/size 缓存，每次 guard 全量重读。`batch.json.node_modules.coverage` 保存全部发现的 node_modules、workspace_packages 与实际 covered_roots。基线之外的全部祖先若存在 node_modules（含断链），输出 WARNING 并 FAIL，防止可选依赖从仓外解析；当前正常基线没有这种布局。

Rust selector admission 同时检查 Python 当前 cwd、批次/独立输出目录和 CARGO_HOME 的全部词法及 realpath 祖先。除当前仓库根的唯一 Git 跟踪 selector 外，任何 `rust-toolchain` / `rust-toolchain.toml`（含断链）都 FAIL；根文件含 `path=` 或非数字 release channel 也 FAIL。根据根文件显式设置 RUSTUP_TOOLCHAIN，不接受继承值；构建前与每次 guard 核对 rustc -vV 的 release 与声明一致（如 1.99 接受 1.99.x，1.99.0 只接受精确补丁版）。**本 worktree 的上层主仓也有 rust-toolchain.toml，因此这个嵌套布局按指定严格规则 FAIL；生产批次需使用没有额外祖先 selector 的独立 checkout。** 不豁免上层主仓文件。

B2 Node version probe、枚举器、gateway、UI 与 driver 的环境只允许宿主 PATH、LANG/LC_ALL/LC_CTYPE/TZ 和装置显式生成的 fixture 参数；HOME 指向该遍 fixture home，NODE_PATH、NODE_OPTIONS、外部代理和动态加载变量不继承。显式 fixture 参数不能重新加入 NODE_PATH/NODE_OPTIONS。

缓存来源说明：calibrate 与独立 run 默认仍从**当前 checkout 根**的 `_build/cargo-home/registry/{index,cache}` 复制，不取 git common dir。此 worktree 的默认路径是 `/Users/da/ws/pilotdeck-rs/_build/wt-b2f/_build/cargo-home`，与协调者预取的 `/Users/da/ws/pilotdeck-rs/_build/cargo-home` 不同。在无额外祖先 selector 的独立 checkout 开批前，将协调者已预取的 registry/index、registry/cache 复制到该 checkout 的 `_build/cargo-home/registry/`（普通目录，不能用符号链接）；不复制 src、git、配置或凭据。干净临时 Batch.build 验证使用该预取来源的副本。

B3 前置改装（#59 / #80 / #53 / #88，#89 P2）：Rust 路由清单固定读取 `specs/ui-server-parity/rust-routes.json`，不提供可替换路径；schema=1，routes 数组元素为 `{method, path, handler:"rust"}`，method 为大写 HTTP verb 或 ALL，path 为 runtime/matrix 的路由模板。空数组表示目前全部由 TS 处理。Rust 侧接管配置/路由表须同步静态导出，审批字段与 FROZEN 共用检查函数，但 Rust 路由豁免必须命中固定白名单 `specs/ui-server-parity/human-approvers.txt` 并有三份依据。白名单每行一个身份，初始仅 `zhengdaqi`；空行与注释行（规范化后以 `#` 开头，含前导空白、零宽字符与全角 `＃`）跳过，行内出现其他 `#` 则整份白名单作废（fail-closed，见 #94.1）；审批人与每行身份同样经过 NFKC + casefold + 去除全部空白及不可见字符，再作非空精确匹配。不可见字符口径见 acceptance.md。白名单外具名人、模型或角色名称、空值均 FAIL；`zhengdaqi` 的零宽字符变体规范化后相等则预期 PASS。只有 FROZEN 沿用 R5 原审批规则（具名人或精确 AI 代批串），不受路由白名单限制。没有用例的豁免也过门。静态路由表和审批人白名单与规约共同进入装置清单、FROZEN 及 capture hash；改白名单即装置变更，须评审，未更新 FROZEN 哈希则 FAIL。

`du` 仅对每行均为 ENOENT 的错误重试三次；耗尽取四次输出总量的最大值，stderr 与 space_check.events 同时记录 approximate、各次字节数。初始 space_estimate 及后续 space_checks 写入 batch.json，不把近似测量冒充精确值。

基线行为出处：`ws/lib/websocket.js` 的 closeTimeout 默认 30000ms；反代只转发实际 Ping/Pong/Close。`ws/lib/websocket-server.js:524` 的 abortHandshake 直接写 socket，不带 Date；反代两种 WS 模式的握手中止 400 均不加 Date，其他本地 400/500/502 仍加 Date。HOME、五个 XDG 目录、TMPDIR 均指向 fixture，PILOTDECK_SKIP_BROWSER_OPEN=1。

回源：[基线 pnpm 10.32.1 workspace 源码](https://github.com/pnpm/pnpm/blob/v10.32.1/workspace/find-packages/src/index.ts#L48-L58) 以 pnpm-workspace.yaml 为成员清单；packages 缺省时沿用 [v10 默认](https://github.com/pnpm/pnpm/blob/v10.32.1/fs/find-packages/src/index.ts#L21-L30) 的全部项目（忽略 node_modules/bower_components）；[rustup 工具链解析源码](https://github.com/rust-lang/rustup/blob/master/src/dist/mod.rs#L130-L168) 允许主.次或精确补丁 release。本次采用 pnpm 优先，不与 package.json workspaces 合并；实际链接目标仍完整绑定。

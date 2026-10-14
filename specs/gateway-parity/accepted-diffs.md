# Gateway 已接受差异（草稿，Issue #23 / A2）

当前 **1 条已接受差异**（A4-FILE-TOCTOU-001，见文末）。透明代理的八组场景必须 100% 一致。归一化规则只处理运行元数据，不把代理行为差异写成豁免。

基线协议重连后 elicitation_respond.delivered=false 是两侧共同的已知业务行为，驱动明确断言并披露；不计作被测侧已接受差异，也不宣称重连后的轮次完成或 UI 自动恢复成功。

A1 的 RSV/continuation/unmasked codec 差异由 A2 原始 WS 探针实际判 FAIL，已通过透明字节隧道修正；修复前原件仍留作回归依据。没有登记允许差异来变绿。

未来追加条目必须包含类别（bug 修复/有意改进/不可避免）、基线依据、具体字段/场景、机器判据和批准记录；禁止概括性忽略整个 method、所有 error、所有时间字段。A4 本地应答的计分还须上游观测点证明 request 未转发，输出相等不能代替本地实现证明。

## A4-FILE-TOCTOU-001 · project_file_read 检查后符号链接替换（原版缺陷 · 安全）

- **类别**：bug 修复（意向书「bug 默认修」第③项：有安全问题）。
- **基线依据**（cc7c4e6，`src/` 下）：`cli/createLocalGateway.ts:1105-1107` 先 realpath 得 canonical 字符串并检查在项目根内；`:1109` 按该字符串 `stat`、`:1111` `readFile`，均默认跟随符号链接，没有根描述符、NOFOLLOW 或与检查绑定的 fd；`tool/builtin/filesystem/pathSafety.ts:113-116` 只检查路径关系。检查与读取之间若路径分量被替换成指向根外的符号链接，TS 会读到根外文件。
- **Rust 行为**：以项目根目录 fd 为锚，逐组件 `openat` + `O_NOFOLLOW`；检查后被替换的链接一律拒绝，不读根外内容，也不回退给 TS。
- **范围**：仅 `project_file_read`（以及后续沿用同一读取路径的本地 method，须各自另登记）在「检查后、读取前路径被替换」的竞态下的结果。不竞态的正常路径（含根内绝对路径与 `..`）与 TS 逐字节一致（60/60 原件）。
- **机器判据**：`src/pilotdeck-gateway/src/project_files/tests.rs` 的 `checked_path_replacements_cannot_read_outside_or_fall_back_to_ts`——同一组 Node 实测竞态探针中，TS 读到 `OUTSIDE_SECRET`，Rust 必须拒绝且不转发；A2 透明代理与本地应答场景无竞态，仍须 100% 一致。
- **批准记录**：人定 2026-10-15（issue #68 选 A），见 `state/decisions-20261014.md`「人的回复」。

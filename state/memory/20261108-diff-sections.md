# 新增差异条目的段名

2026-11-08 CLI 开发中三次出现把“Rust 行为”和“范围”合成一个段名的候选。新增 `specs/gateway-parity/diffs/*.md` 一律分别使用：`类别`、`基线依据`、`Rust 行为`、`范围`、`机器判据`、`批准记录`。提交前运行 `python3 -I scripts/gateway-parity/check_frozen.py --base origin/main` 并等待进程完成，只有实际退出0才能进入提交步骤；不能收到session ID就继续提交。未人批差异明确写未批准/非已接受差异，不改冻结判分。

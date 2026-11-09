# TUI 输入与粘贴

这些自写脚本启动隔离 HOME/PILOT_HOME/project、动态 loopback fake Gateway 和真实 CLI/PTY。只在 mbp2 运行，不接用户的 18789 或真实 home；固定 Node 22.22.0 / ICU 78.2 与 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85`。每轮 `--out` 必须是不存在的目录，保存终端原始字节、wire、退出及子进程记录。

```sh
python3 smoke.py --bin "$bin" --node "$node" --ts-root "$ts" --out "$new_output/edit"
python3 stress.py --bin "$bin" --node "$node" --ts-root "$ts" --out "$new_output/stress" --case unterminated
python3 boundary.py --bin "$bin" --node "$node" --ts-root "$ts" --out "$new_output/boundary" --case raw-crlf
python3 signals.py --bin "$bin" --node "$node" --ts-root "$ts" --out "$new_output/signals"
```

`smoke.py` 的 13 格覆盖左右/退格/Delete、中文、emoji、组合字素、分块组合输入、CRLF 粘贴、拆开的起始标记、SS3 和 Alt；CSI Delete 按基线向左删除。`stress.py` 的四种 `--case` 是 `unterminated / oversize / help-overflow / active-overflow`，实际发送完整 128KiB，核对仍能取消、收事件、返回正常交互且没有误提交。不是 RSS 上限测试。

`boundary.py` 的九种 `--case` 是 `empty / exact / over / permission-overflow / permission-raw / raw-crlf / late-marker / long-csi / active-long-csi`。包含 65536/65537 字节边界、权限画面中的粘贴不能当作 y/n、同次 read 的 CRLF 不提前提交、迟到粘贴标记和超长 CSI 的丢弃/取消/恢复。

`signals.py` 在未完成粘贴中发送 TERM/INT/HUP，每种信号覆盖共用 stdin/stdout open-file description 的 NONBLOCK=0/1，以及分离描述的四种组合，共 18 格，核对预期信号退出、termios、NONBLOCK 与关闭 bracketed-paste。记录完整 F_GETFL 原值；`flags-oracle.py <new-output>` 的独立正控证明 Darwin 普通 PTY 写入即可把 2 变为 65538，此位不是 NONBLOCK，不能把全值变化误判为恢复失败。

`ts/capture-edit.py` 与 `ts/capture-paste.py` 使用 `--node / --ts-root / --out`，实际跑固定 TS 的八种编辑与三种粘贴。`ts/raw/probe.py` 同参数，采集同批 `hello\r / y\r / q\r` 与之后单键的区别；结果是观察记录，不能把脚本退出0当成所有行为都正确。修改仅限自写驱动路径参数，没有复制或修改 TS 产品代码。原始采集脚本和逐字节证据保留主仓 `_tmp/20261108-cli-tui/input-ts-probe/mbp2-01/` 与 `input-implementation/raw-arbitration/`。

最终 mbp2 input-r5 严格 clippy、35 库测试及 build 通过，一个 ignored helper 由父测试实际调用；bin SHA256 `6649fb945d08a727114e217a26b34cd28f2a9e7dc49a16a5fcbc8476f9f1f18d`。新输入 26 格、信号 18 格、旧交互 18 格均通过；旧交互含 `/new` 和迟到 abort 的精确草稿、侧栏打开时批量追加文字、权限、背压、工具、帮助与显示。证据与源码/bin 指纹在主仓 `_tmp/20261108-cli-tui/input-implementation/r5-evidence/`。此前编译、全 F_GETFL 比较、旧回归暴露的草稿丢失原件均保留，不能用早轮通过替代当前源码验证。

输入按 Unicode 字素编辑；原 TS UTF-16 半个代理项及组合字符删除差异、资源边界与未完成标记处理见 `specs/gateway-parity/diffs/D4-TUI-INPUT-001.md`。未标记文字只能按一次有界 read 识别，不能保证任意分包仍是一批粘贴；跨包多行应使用终端 bracketed paste。独立 Esc 有30ms判别窗口，迟到起始标记会弃置正文，先前 Esc 的动作无法撤销。真实 provider、native 权限/history 和人眼体验不由这些 fake Gateway 测试代填。

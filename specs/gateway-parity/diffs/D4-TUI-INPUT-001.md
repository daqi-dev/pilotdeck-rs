---
id: D4-TUI-INPUT-001
---

## 类别

Unicode 输入编辑修复、终端粘贴资源边界与中断恢复。候选差异，不取得冻结判分豁免。

## 基线依据

固定 TS `cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85` 的真实 CLI/PTY，在 mbp2 固定 Node22.22.0/ICU78.2 下采得八编辑、三粘贴原件，位于主仓 `_tmp/20261108-cli-tui/input-ts-probe/mbp2-01/`。ASCII/中文左右与退格、CSI Delete 向左删除、完整同批 CRLF 与 bracketed paste 的正文保留可实测。TS 在 emoji 中间删除/插入会产生孤立 UTF-16 代理项；组合字符 `AéB` 在 B 前退格得到 `AeB`，Rust 按字素得到 `AB`，后者也须列为差异，不能只用代理项修复概括。

同批 `hello\r` 不提交、另发 Enter 才提交；权限 `y\r` 和帮助 `q\r` 同批不触发操作，单发 y/q 才触发。三格实际 TS 仲裁在 `input-implementation/raw-arbitration/`，不推广到所有分包。TS PromptInput 的 isRunning 只改变边框、仍允许编辑；TS 运行中 Enter 会清草稿后拒绝提交，Rust 保留草稿到可提交是已有差异。

早期 Rust Crossterm reader 的128KiB无结束标记探针，在两次Ctrl-C后两秒观察窗仍未退出，WS pong和FLOW_MARKER仍到达，SIGTERM可恢复；不将两秒观察夸大为无限挂起或RSS实测。原件在主仓 `paste-bound-probe/`。

## Rust 行为

左右移动、向左删除和插入按 Unicode 字素边界，插入/删除后重新校准游标。输入总量64KiB，超限明确拒绝整条提交。用有界非阻塞字节读取替代Crossterm事件reader；每次读最多4096字节、解析最多4096步、待解析队列约8KiB、单次bracketed正文最多64KiB，仍用Crossterm恢复终端和输出控制。

完整单read的UTF8文字及CR/LF（不含其他控制字节、长度大于1）作为原子草稿，不把正文回车变成提交；普通文字在busy RPC、取消等待或侧栏中继续编辑，提交仍由既有状态门控制，help/viewer/权限画面不执行批量正文。跨read不承诺重组未标记粘贴。

bracketed paste只在始末可编辑且session/turn generation未变时插入；模态或活动期间的正文不执行权限/命令，超限丢弃至结束标记。未闭合/超限时首次Ctrl-C取消粘贴，后续Ctrl-C仍可取消回合或退出；超长普通CSI按其自身终结符恢复。独立Esc用30ms消歧，之后才出现的粘贴标记弃置正文，已执行Esc动作无法撤销。正常、错误和信号退出恢复termios及原有stdin/stdout NONBLOCK设置。

## 范围

Unix远端TUI输入与退出恢复。不修改Gateway协议和权限决定，不代表任意终端编码、native权限/history、默认local fallback、完整D4或CLI已经完成；资源计数不是进程RSS上限。

## 机器判据

`tests/fixtures/tui-input/README.md` 列完整复现。mbp2 input-r5 strict clippy、35库测试（另1辅助由父调用）、build通过；实际bin SHA256 `6649fb945d08a727114e217a26b34cd28f2a9e7dc49a16a5fcbc8476f9f1f18d` 的26输入、18信号和18旧交互均通过。包括/new及abort等待草稿保全、侧栏批量输入、65536/65537/128KiB、权限不误触发、超长CSI、EOF单测和终端背压。

源文件与回传原件哈希在主仓 `_tmp/20261108-cli-tui/input-implementation/r5-evidence/`。r4的信号全F_GETFL比较是假失败：独立Darwin PTY纯写正控可重复2→65538；最终测试保留全数值并核真实修改的NONBLOCK位及分离OFD组合。r4/new草稿丢失是真失败，已通过独立TextBatch修复并重跑，原件保留。

## 批准记录

2026-11-08，AI代批，待人复核。用户授权完整CLI及不要求逐像素重构；本文件仅登记候选Unicode/安全/终端行为，未当作已批准冻结差异。独立代码复核与mock实测不代替正式合并闸门、真实provider或真人体验签字。

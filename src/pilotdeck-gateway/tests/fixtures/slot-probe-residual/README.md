# #2516 槽位探针残留负控

针对 #2505 fix-r3 四条残留（#2516）的负控组，每条修法一个变异体：

1. `pgrep-stderr-strip-restored`——rc=1 的 stderr 判空恢复 `.strip()`，
   纯空白 stderr 被当成干净无匹配（item 1）。
2. `pgrep-single-sweep-restored`——两轮全名单清扫退回单轮，第二轮才
   alive 的入口被漏读（item 2）。
3. `ps-text-mode-restored`——ps 取样恢复 `text=True`，universal newlines
   把字面 `\r` 折成 `\n` 撕行，撕出的 `int int rest` 行改写父表（item 3；
   fix-r1 起探针取样带 `NEGCTL_PROBE_TIMEOUT_SECONDS` 上限，锚点与
   变异体同载该行，恢复的缺陷只有 text=True 本身）。
4. `numeral-comment-restored`——探针注释恢复「ten clean no-matches」
   数词写法（item 4；判据是数词正则（数字或 zero..twelve + `clean
   no-match`），不再钉死上一次缺陷的两处拼写）。

fix-r1（PR #2587 r0 评审）追加三条：

5. `pgrep-stdout-census-dropped`——pgrep 控制字符普查去掉 stdout 半边，
   rc=0 带撕行 stdout 仍被读成 alive（P2-a 4；`pgrep-stdout-control-char-fails`
   判 `"control"` 消息，与 rc=1-with-stdout 闸的消息可区分）。
6. `ps-timeout-swallowed`——ps 超时分支恢复 `(0, "")`，挂住的取样被读成
   「无 cargo」（P2-a 1；假 ps 支持 `sleep` 字段，配合每测试
   `NEGCTL_PROBE_TIMEOUT_SECONDS` 覆盖）。
7. `pgrep-timeout-swallowed`——pgrep 超时同形（P2-a 1）。

另有 pin（基线正控，不绑变异体）：`pgrep-second-sweep-failure`——第一轮
干净、第二轮 rc=1 带 stdout 必须判探针失败（P2-a 2）；`ps-hang-times-out`
/ `pgrep-hang-times-out`——超时分支的固定读数。

执行表先对静态 `EXPECTED_NAMES` 名册（`negative_selection.registered`，
七名按表序）核验后才跑任何模式，删行 / 改名 / 清空名册都是启动拒跑
（退出码 2），空表 0/0 的假绿由 `verdict_exit` 封死（fix-r1 P1-2，#1666
同型）。

变异体不经磁盘：`negative_process.published_module` 在内存里发布编译后的
变异源码（apply→kill→restore 即 publish→断言失败→unpublish），跑完校验探针
文件字节未变。被点名测试通过生成进系统临时目录的假 `ps` / `pgrep`
（经 `PDRS_2516_FAKE_SPEC` JSON 驱动、pgrep 按名字计次以复现两轮语义、
`{CALLER_PID}` 以 `os.getppid()` 替换、两者均可 `sleep` 楔住以驱动超时
分支）驱动**真实** subprocess 管道——text=True 变异体被杀时 `got (0, '')`
正是真实撕行后的 fail-open 读数。锚点过期（片段在探针文件中不恰好出现
一次）以退出码 2 拒跑，绝不记成 SURVIVED。

```sh
# 全量：基线 17 例 + 七杀 + 磁盘不变校验（退出 0 = 7/7 KILLED）
python3 src/pilotdeck-gateway/tests/fixtures/slot-probe-residual/negative-controls.py
python3 …/slot-probe-residual/negative-controls.py --check-anchors  # 仅锚点预检
python3 …/slot-probe-residual/negative-controls.py --smoke          # 真 ps/pgrep，无假件
```

全量跑纯 Python、零 cargo（实测 <5 s），CI 在 `ci.yml` 的纯 Python 负控
段直接跑全量（fix-r1 P2-a 5）。macOS / Linux（procps 4.0.4）宿主 `ps -Aww`
对 argv 控制字节的转义实测与 item 3 保守判法的依据见
`_tmp/20261026-issue-2516/fix-r1/RESULT.md`（仓库证据目录，不进提交）。

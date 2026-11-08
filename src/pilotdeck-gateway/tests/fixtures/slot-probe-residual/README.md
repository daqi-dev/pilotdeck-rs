# #2516 槽位探针残留负控

针对 #2505 fix-r3 四条残留（#2516）的负控组，每条修法一个变异体：

1. `pgrep-stderr-strip-restored`——rc=1 的 stderr 判空恢复 `.strip()`，
   纯空白 stderr 被当成干净无匹配（item 1）。
2. `pgrep-single-sweep-restored`——两轮全名单清扫退回单轮，第二轮才
   alive 的入口被漏读（item 2）。
3. `ps-text-mode-restored`——ps 取样恢复 `text=True`，universal newlines
   把字面 `\r` 折成 `\n` 撕行，撕出的 `int int rest` 行改写父表（item 3）。
4. `numeral-comment-restored`——探针注释恢复「ten clean no-matches」
   数词写法（item 4）。

变异体不经磁盘：`negative_process.published_module` 在内存里发布编译后的
变异源码（apply→kill→restore 即 publish→断言失败→unpublish），跑完校验探针
文件字节未变。被点名测试通过生成进系统临时目录的假 `ps` / `pgrep`
（经 `PDRS_2516_FAKE_SPEC` JSON 驱动、pgrep 按名字计次以复现两轮语义、
`{CALLER_PID}` 以 `os.getppid()` 替换）驱动**真实** subprocess 管道——
text=True 变异体被杀时 `got (0, '')` 正是真实撕行后的 fail-open 读数。
锚点过期（片段在探针文件中不恰好出现一次）以退出码 2 拒跑，绝不记成
SURVIVED。

```sh
# 全量：基线 13 例 + 四杀 + 磁盘不变校验（退出 0 = 4/4 KILLED）
python3 src/pilotdeck-gateway/tests/fixtures/slot-probe-residual/negative-controls.py
python3 …/slot-probe-residual/negative-controls.py --check-anchors  # 仅锚点预检
python3 …/slot-probe-residual/negative-controls.py --smoke          # 真 ps/pgrep，无假件
```

macOS 宿主 `ps -Aww` 对 argv 控制字节的转义实测与 item 3 保守判法的依据
见 `_tmp/20261026-issue-2516/RESULT.md`（仓库证据目录，不进提交）。

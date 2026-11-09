# SDK fork-local MCP 固定 TS 采集

capture.mjs 是自写 AST driver，不含复制的基线源码。它在内存中 transpile 并调用固定 TS 的 resolveSdkSubagentMcpServers、真实 DialogGatewayError 与 sessionFactory.getSdkMcpServers 回调；不构造完整 Registry/Session bundle，不启动 MCP/provider。

输入支持 `node capture.mjs BASELINE OUT_DIR`，或 `PDRS_BASELINE=BASELINE node capture.mjs OUT_DIR`。BASELINE 必须指向固定只读 checkout cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85，具有其原 node_modules。driver 校验四个原文件 SHA、TypeScript 5.9.3、Node v22.22.0、ICU 78.2 以及 PDRS_VERIFIED_TS_SHA。该 SHA 应由调用方在 sandbox 外 `git -C "$BASELINE" rev-parse HEAD` 核实后传入，不可拿环境声明代替 checkout 核验。

OUT_DIR 的父目录必须存在，OUT_DIR 必须不存在；driver 独占创建 0700 输出目录，oracle.json 使用 wx 创建，不覆盖证据，也拒绝在基线内部输出。采集必须在 Darwin sandbox-exec 下拒绝所有网络及 process-fork，并仅允许独占 scratch 目录写入；sandbox-exec 缺失应明确失败，禁止裸跑 fallback。固定 Node 的绝对路径由调用方传入，不在脚本内硬编码机器路径。

可复现流程（BASELINE、NODE、DRIVER 均为调用方提供的绝对路径）：

```sh
set -eu
EXPECTED=cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85
VERIFIED=$(git -C "$BASELINE" rev-parse HEAD)
test "$VERIFIED" = "$EXPECTED"
test -x /usr/bin/sandbox-exec
SCRATCH=$(mktemp -d)
export SCRATCH
mkdir "$SCRATCH/home" "$SCRATCH/tmp"
/usr/bin/python3 - <<'PROFILE'
import json,os
from pathlib import Path
p=Path(os.environ['SCRATCH']).resolve()
(p/'sandbox.sb').write_text('(version 1) (allow default) (deny network*) (deny process-fork) (deny file-write*) (allow file-write* (subpath '+json.dumps(str(p))+'))\n')
PROFILE
HOME="$SCRATCH/home" PILOT_HOME="$SCRATCH/home/pilot" TMPDIR="$SCRATCH/tmp" \
PDRS_VERIFIED_TS_SHA="$VERIFIED" \
/usr/bin/sandbox-exec -f "$SCRATCH/sandbox.sb" "$NODE" "$DRIVER" "$BASELINE" "$SCRATCH/capture" \
  >"$SCRATCH/stdout" 2>"$SCRATCH/stderr"
shasum -a 256 "$SCRATCH/capture/oracle.json"
test ! -s "$SCRATCH/stderr"
```

输出包括 16 个 resolver 用例及两个 callback 查询：enabled/disabled/missing reference，inline，重复 string/inline 的顺序差异，mixed 与空输入；保留 keys 顺序、structuredClone 身份/输入不变断言及真实错误 name/code/message/details。栈在首次采集前已明确不进入投影，无时间、随机路径或其他事后归一化。AST start/end 是 TS UTF-16 字符索引，非 UTF-8 字节偏移。所有 warning 均失败。

本次 mbp2 真实重采：argv 与 PDRS_BASELINE 两种方式均 exit 0、stderr 空，16 rows；新 oracle 与原 oracle 逐字相同，SHA256 a2f5740f70bd8edc49d31b05536ff1d7beb9ce89c5353a89f6c2bcc547a29bda。driver SHA256 3a65a9bd0b9eb85d55ace7bd3a0e7d1f910e22287754c740f3b897afede40333。首次 portable import 名称冲突的失败原件另留 run-argv，未覆盖或计作通过。

可入库文件为本 README 与 capture.mjs；oracle.json 使用已入库的同 SHA 固定原件，不新增另一份真相源。此采集只证明真实解析合同，不证明 child endpoint 生命周期；生命周期由独立真实 native probe 验证。

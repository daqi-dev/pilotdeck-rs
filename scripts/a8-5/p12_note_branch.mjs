/**
 * p12 note 分支选择纯函数（#2989 fix-r8 sibling#4，参照 p16 fix-r3 先例：
 * note/守门纯函数抽出入仓 + 微测直调真实代码，不复制逻辑）。p12 主文件顶层有
 * env/argv 解析与 main() 无条件调用（import 即跑探针），故纯函数落本模块——
 * 探针与微测（p12_note_microtest.mjs）import 同一模块，判的是探针实际走的分支序。
 * 判废面矩阵 M1–M15 正本见 docs/experiments/exp016-a85-switch-matrix-probes/
 * readme.md p12-window-spec；判废条件照 specs/gateway-parity/diffs/
 * A8.3-CH-SMS-012-R1.md:121-134。
 *
 * fix-r8 boundary#1 + sibling#1（两家独立撞上同一缺口，结构性修法 = 短路前置）：
 * 旧分支序把 HEADERS_TIMEOUT_NOMINAL 只接到「支持②」三分支，settle 分支与
 * firstByteMs≠null 分支在 headersTimeout≠60000（矩阵 M11 判废面，R1:124）时仍输出
 * 机制性结论（「≥60s 相位窗内（与 408 驱动面一致）」/「排除『close 清 checker ⇒
 * 408 不发』」）——对按 M11 已整体判废的产物输出机制结论，与 p12-window-spec
 * 「探针不输出支持性结论、只报事实」相反。本模块把 M11 判定提到一切 note 分支
 * 之前：≠60000 一律走 m11-invalidated 事实分支。
 *
 * 分支序（与 p12_zero_byte_close.mjs note 模板序一致）：
 *   m11-invalidated → settle → bytes-exclude → support2 → fact → right-censored。
 * M11 前置于 M7（<95 s 右删失句）= readme:76「任一 M2/M11/… 命中即作废」的优先级：
 * 该形状产物按 M11 判废，且 M11 句只报事实、与窗长无关（boundary#3）。
 */
// R1:124 判据：headersTimeout 须 =60000（Node 默认；与 p12 头注 :24 同源）。
export const EXPECTED_HEADERS_TIMEOUT_MS = 60_000;
// ≥95 s 窗已覆盖最坏相位 408 落点（相位窗 (60, 90] s + 毫秒级 tick 抖动）。
export const WORST_PHASE_FLOOR_MS = 95_000;

/**
 * note 分支选择。入参全部为 observation/scenario 行已有字段（快照值）：
 * - closeSettled：观测窗内 server.close() 是否落定；
 * - firstByteMs：首个服务端字节相对 close() 起算的毫秒（null = 无字节）；
 * - headersTimeout：scenario 行两层实读命中的值（数字；读不到的形状在
 *   serverShape=unreachable 时已 exit 4，不进本函数）；
 * - windowMs：观测窗毫秒；
 * - bytes/socketEventCount/socketLocalStillOpen：fix-r5 结论谓词四条件的其余三项。
 * 返回分支名，note 模板由探针按分支名拼装。
 */
export function p12NoteBranch({
  closeSettled, firstByteMs, headersTimeout, windowMs, bytes, socketEventCount, socketLocalStillOpen,
}) {
  if (headersTimeout !== EXPECTED_HEADERS_TIMEOUT_MS) return "m11-invalidated";
  if (closeSettled) return "settle";
  if (firstByteMs !== null) return "bytes-exclude";
  if (windowMs >= WORST_PHASE_FLOOR_MS) {
    return bytes === 0 && socketEventCount === 0 && socketLocalStillOpen
      ? "support2"
      : "fact";
  }
  return "right-censored";
}

/**
 * p12 note 分支纯函数的入仓负控微测（#2989 fix-r8 sibling#4，参照 p16 fix-r3
 * 先例：直调 import 的真实代码，不复制逻辑）。被测 = scripts/a8-5/p12_note_branch.mjs
 * 的 p12NoteBranch（p12_zero_byte_close.mjs 的 note 分支序整体收敛于此，fix-r8
 * boundary#1 + sibling#1 短路前置）。纯函数直调：无参数、无端口、无落盘副作用、
 * 不依赖 TS 基线 dist；同步执行（无 watchdog 必要——无 IO/无 await）。
 * 运行：node scripts/a8-5/p12_note_microtest.mjs；退出码 0 = 全过，任一断言
 * 失败收集后汇总 exit 1。
 * 格点（判杀面 = 分支序变异）：
 *   T1  60000 + 四条件 + ≥95 s 窗 ⇒ support2（「支持②」正控——fix-r7 语义不回退）
 *   T2  ≠60000 + 同四条件 ⇒ m11-invalidated（sibling#4 负控正主：自守谓词被
 *       误删/恒真时必红——HEADERS_TIMEOUT_NOMINAL 只盖「支持②」分支的旧序必红）
 *   T3  ≠60000 + settle ⇒ m11-invalidated（M11×settle：settle 分支不得抢在自守前，
 *       旧序输出「≥60s 相位窗内（与 408 驱动面一致）」机制结论——必红）
 *   T4  ≠60000 + 有字节 ⇒ m11-invalidated（M11×bytes：旧序输出「排除『close 清
 *       checker ⇒ 408 不发』」——必红）
 *   T5/T6/T7/T8  nominal 各剩余分支（settle / bytes-exclude / fact / 右删失）
 *   T9  ≠60000 + <95 s 窗 ⇒ m11-invalidated（M11 前置于 M7——任一判废命中即作废）
 *   T10 ≠60000 + 早 settle（M2 并存形状）⇒ 仍 m11-invalidated（invalidationHit 是
 *       独立字段，note 分支不因 M2 并存而改变）
 *   T11 载重常量：EXPECTED_HEADERS_TIMEOUT_MS=60000（R1:124）、
 *       WORST_PHASE_FLOOR_MS=95000（防常量漂移静默改变判废/相位窗口径）
 */
import assert from "node:assert/strict";
import {
  p12NoteBranch, EXPECTED_HEADERS_TIMEOUT_MS, WORST_PHASE_FLOOR_MS,
} from "./p12_note_branch.mjs";

const failures = [];
const cell = (id, actual, expected) => {
  try {
    assert.equal(actual, expected);
    console.log(`ok #T${id} ${actual}`);
  } catch (e) {
    failures.push(`T${id}: ${e.message}`);
    console.log(`FAIL #T${id} ${e.message}`);
  }
};

// 「支持②」四条件基型（fix-r5 谓词；fix-r7 第五条件 = headersTimeout=60000）
const four = { closeSettled: false, firstByteMs: null, bytes: 0, socketEventCount: 0, socketLocalStillOpen: true };

cell(1, p12NoteBranch({ ...four, headersTimeout: 60_000, windowMs: 96_000 }), "support2");
cell(2, p12NoteBranch({ ...four, headersTimeout: 3_600_000, windowMs: 96_000 }), "m11-invalidated");
cell(3, p12NoteBranch({ ...four, closeSettled: true, headersTimeout: 3_600_000, windowMs: 96_000 }), "m11-invalidated");
cell(4, p12NoteBranch({ ...four, firstByteMs: 61_000, bytes: 64, headersTimeout: 3_600_000, windowMs: 96_000 }), "m11-invalidated");
cell(5, p12NoteBranch({ ...four, closeSettled: true, headersTimeout: 60_000, windowMs: 96_000 }), "settle");
cell(6, p12NoteBranch({ ...four, firstByteMs: 61_000, bytes: 64, headersTimeout: 60_000, windowMs: 96_000 }), "bytes-exclude");
cell(7, p12NoteBranch({ ...four, socketEventCount: 1, headersTimeout: 60_000, windowMs: 96_000 }), "fact");
cell(8, p12NoteBranch({ ...four, headersTimeout: 60_000, windowMs: 70_000 }), "right-censored");
cell(9, p12NoteBranch({ ...four, headersTimeout: 3_600_000, windowMs: 70_000 }), "m11-invalidated");
cell(10, p12NoteBranch({ closeSettled: true, firstByteMs: null, bytes: 0, socketEventCount: 0, socketLocalStillOpen: false, headersTimeout: 3_600_000, windowMs: 70_000 }), "m11-invalidated");

cell(11, EXPECTED_HEADERS_TIMEOUT_MS === 60_000 && WORST_PHASE_FLOOR_MS === 95_000 ? "constants-ok" : `constants-drift(expected 60000/95000, got ${EXPECTED_HEADERS_TIMEOUT_MS}/${WORST_PHASE_FLOOR_MS})`, "constants-ok");

if (failures.length > 0) {
  console.error(`p12_note_microtest FAILED（${failures.length} 格红）:`);
  for (const f of failures) console.error(`  ${f}`);
  process.exit(1);
}
console.log("p12_note_microtest DONE（T1–T11 全过）");
process.exit(0);

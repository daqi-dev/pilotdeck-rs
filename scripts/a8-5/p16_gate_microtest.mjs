/**
 * P16 守门/note 纯函数的入仓负控微测（#2759 fix-r3，评审 sibling#4：仓内须有
 * import p16_stop_acceptance.mjs 真实代码的负控测试，不复制逻辑）。直调本文件导出的
 * 纯函数 + common 的 keepAliveProbe（本地 node:http server，不依赖 TS 基线 dist）。
 * 每条断言带 #T<N> 专属标记，供变异负控绑定判杀；全部 section 顺序执行、失败不
 * 中止（收集后统一汇总，任一失败退出码 1）。
 * 运行：node scripts/a8-5/p16_gate_microtest.mjs（无参数、无落盘副作用——p16 的
 * 路径/Recorder 惰性初始化于 main()，import 本体无 mkdir/argv 副作用，fix-r3）。
 * fix-r4：T4 的 listen 挂 error 监听（EADDRINUSE → 受控 FAIL 进 failures，不再
 * 未捕获异常/挂起）；总时限 watchdog 60s（超时打印已收集 failures 后 exit 2，防任一
 * section 挂起导致进程无出码）；T13/T14 新增（守卫纯函数三分支 + 直接执行正控——
 * 守卫误判为非 main 时 T14 必红，评审 boundary#1）。
 * 端口纪律：39994（微测专用——fix-r4 挪出 39871–39898 段：原 39894 与 p15_driver 的
 * A-elevate 用例撞号、39895 为 p15 B-control，避让探针 39896–39898；39994 全仓无脚本
 * 绑定，评审 sibling#1 / concurrency#1）。
 * fix-r5（评审 boundary#1 P1，当轮必修）：T15 **导出纯函数直调格**——直调
 * entryHrefCandidates、真实文件系统形状（symlink/空格路径/目录/不存在路径）实跑归一；
 * 判杀面是**函数体内**变异（返回字段互换 / 去 realpathSync 归一 / pathToFileURL 误用）。
 * fix-r6（评审 sibling#2，P1）声明订正：T15 不经 p16_stop_acceptance.mjs 模块作用域的
 * 真实调用点（v22.22.0 直接执行命中 import.meta.main===true 首分支、process.argv 实参
 * 不参与比较）——调用点接线（含 T16 的 else-if 消费）零运行时覆盖，登记待办（coord
 * 跟进单）。fix-r6 另修（评审 sibling#1/concurrency#1，P1 + concurrency#3，P2a）：新增
 * T16（isSelfEntryPath 直调格——分支③独立复检的纯函数面）；T14 直跑负控格临时 scratch
 * finally rmSync 清场 + 清场断言（对齐 T15）；末行登记随 T1–T16。
 * T15 全部同步 fs 操作（无子进程/无网络），watchdog 预算不变。
 */
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { keepAliveProbe, keepAlivePremise } from "./common.mjs";
import {
  parseRawGate, pooledHeaderPremise, derivePooledPremise,
  sawCleanFin, eventsAfterWrite, silentHang, isAbnormalEvent,
  rawReuseNote, reuseSilentHangClause, partBNote,
  pooledSkipReason, agentReuseNote, isMainEntry, entryHrefCandidates, isSelfEntryPath,
} from "./p16_stop_acceptance.mjs";

const PORT_MT = 39994;
const failures = [];
const section = async (name, fn) => {
  try {
    await fn();
  } catch (e) {
    failures.push(`[${name}] ${e?.message ?? e}`);
  }
};

// fix-r4（sibling#1 / concurrency#1）：总时限 watchdog——任一 section 挂起（事件循环
// 活着的 socket/timer 挂起，或纯 pending promise 使顶层 await 不决）时进程仍必须有
// 受控结论：到点打印已收集的 failures 后 exit 2（区别于断言失败的 exit 1）。不 unref
// ——drained-loop 挂起也要被钉住到点触发；正常收尾路径（下方汇总后）clearTimeout，
// 不拖长正常跑。子进程全部为 spawnSync（同步、自带 timeout），触发时无存活子进程；
// 触发即 process.exit，watchdog 自身计时器随进程终结，不越界存活。
const WATCHDOG_MS = 60_000;
const watchdogTimer = setTimeout(() => {
  console.error(`p16_gate_microtest TIMEOUT（${WATCHDOG_MS}ms watchdog 触发，有 section 挂起）:`);
  if (failures.length > 0) {
    for (const f of failures) console.error(`  ${f}`);
  } else {
    console.error("  （触发前无 section 失败——挂起段不明，见上方各 section 输出）");
  }
  process.exit(2);
}, WATCHDOG_MS);

// T1 格点先全部解析（不设断言），再逐标签独立 section——变异下单格失败不遮蔽其余格
const G = {
  g1: parseRawGate("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: Keep-Alive , X\r\n\r\nok"), // 1a：1.1+显式（多值/OWS）
  g2: parseRawGate("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"), // 1b：1.1+显式 close
  g3: parseRawGate("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"), // 1c：1.1 缺头隐式
  g4: parseRawGate("HTTP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nok"), // 1d：1.0 缺头
  g5: parseRawGate("HTTP/1.0 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok"), // 1e：1.0+显式（对称）
  g6: parseRawGate("HTTP/1.1 200 OK\r\nContent-Length: 2"), // 1f：半截 buf
  g7: parseRawGate("HTTP/1.0 200 OK\r\nContent-Length: 30\r\n\r\nconnection: keep-alive\r\nbody-bytes-here..."), // 1g：体行陷阱正向
  g8: parseRawGate("HTTP/1.1 200 OK\r\nContent-Length: 30\r\n\r\nconnection: close\r\nbody-bytes-here..."), // 1h：体行陷阱反向
  g9: parseRawGate("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: keep-alive\r\nConnection: close\r\n\r\nok"), // 1i：重复 Connection 行（fix-r3）
};
await section("T1a", () => {
  assert.equal(G.g1.rawPremise, true, "#T1a 显式 keep-alive（大小写/OWS/多值）应成立");
  assert.ok(G.g1.connHeader.startsWith("connection:"), "#T1a connHeader 应取自头块");
});
await section("T1b", () => {
  assert.equal(G.g2.rawPremise, false, "#T1b 显式 close 不成立");
});
await section("T1c", () => {
  assert.equal(G.g3.rawPremise, true, "#T1c 1.1 缺头按隐式 keep-alive 成立");
  assert.equal(G.g3.connHeader, undefined, "#T1c 头块无 connection 行");
});
await section("T1d", () => {
  assert.equal(G.g4.rawPremise, false, "#T1d 1.0 缺头不成立");
  assert.equal(G.g4.http11, false, "#T1d http11 应为 false");
});
await section("T1e", () => {
  assert.equal(G.g5.rawPremise, true, "#T1e 1.0+显式 keep-alive 应成立（显式对称）");
});
await section("T1f", () => {
  assert.equal(G.g6.rawPremise, false, "#T1f 头不完整不成立");
  assert.equal(G.g6.headComplete, false, "#T1f headComplete 应为 false");
});
await section("T1g", () => {
  assert.equal(G.g7.rawPremise, false, "#T1g 体行 connection: keep-alive 不得当响应头（1.0 缺头不成立）");
  assert.equal(G.g7.connHeader, undefined, "#T1g 头块无 connection 行（体行不算）");
});
await section("T1h", () => {
  assert.equal(G.g8.rawPremise, true, "#T1h 体行 connection: close 不得当响应头（1.1 缺头隐式成立）");
  assert.equal(G.g8.connValue, undefined, "#T1h 头块无 connection 值");
});
await section("T1i", () => {
  // 重复 Connection 行取全量合并（对齐 node 侧 r.headers.connection 合并为 "keep-alive, close"、
  // keepAliveToken close 优先判 false——实测 v22.22.0，见 fix-r3 轮次证据 emp-misc）
  assert.equal(G.g9.rawPremise, false, "#T1i 重复 Connection 行（keep-alive + close）应按合并值 close 优先不成立");
  assert.equal(G.g9.connValue, " keep-alive, close", "#T1i connValue 应为两行值逗号合并");
  assert.equal(G.g9.connHeader, "connection: keep-alive", "#T1i connHeader 为小写归一首条 connection 行（fix-r4：connHeader/connValue 双落盘的口径钉死）");
  assert.equal(keepAlivePremise({ ok: true, connection: "keep-alive, close", httpVersion: "1.1" }).premise, false, "#T1i 与 node 侧头值同形输入同判（对拍）");
});

// T2 derivePooledPremise 四形（含判别形）
const P2 = {
  okKa: { ok: true, httpVersion: "1.1", connection: "keep-alive" },
  okClose: { ok: true, httpVersion: "1.1", connection: "close" },
  okNoHead11: { ok: true, httpVersion: "1.1" },
  failed: { ok: false, error: "timeout" },
};
await section("T2a", () => {
  const d1 = derivePooledPremise(P2.okKa, ["127.0.0.1:39896:50000"]);
  assert.equal(d1.premise, true, "#T2a direct 真应成立");
  assert.equal(d1.direct, true, "#T2a direct 分量为 true");
});
await section("T2b", () => {
  // 判别形——头说 keep-alive、freeSockets 无条目（无自定界不入池）
  const d2 = derivePooledPremise(P2.okKa, []);
  assert.equal(d2.header, true, "#T2b 头推断为 true");
  assert.equal(d2.direct, false, "#T2b direct 分量为 false");
  assert.equal(d2.premise, false, "#T2b direct 应压过头推断（修前假成立面）");
});
await section("T2c", () => {
  // freeKeys 不可观测（防御性分支：Agent 构造即初始化 freeSockets 为对象，实测
  // v22.22.0 运行时不可达——人工格覆盖）→ 回退头推断
  const d3 = derivePooledPremise(P2.okKa, undefined);
  assert.equal(d3.premise, true, "#T2c freeKeys 不可观测应回退头推断");
  assert.equal(d3.direct, undefined, "#T2c direct 分量为 undefined");
  const d3b = derivePooledPremise(P2.okClose, undefined);
  assert.equal(d3b.premise, false, "#T2c 回退头推断（显式 close）不成立");
  const d3c = derivePooledPremise(P2.failed, undefined);
  assert.equal(d3c.premise, false, "#T2c 请求失败不成立");
});
await section("T2d", () => {
  // 头推断不成立但池内有条目（direct 压过 header，另一向）
  const d4 = derivePooledPremise(P2.okNoHead11, ["127.0.0.1:39896:50001"]);
  assert.equal(d4.header, true, "#T2d 1.1 缺头头推断为 true（对照）");
  const d5 = derivePooledPremise(P2.okClose, ["127.0.0.1:39896:50002"]);
  assert.equal(d5.header, false, "#T2d 显式 close 头推断为 false");
  assert.equal(d5.premise, true, "#T2d direct 真压过头推断 false");
});

await section("T3", () => {
  assert.equal(pooledHeaderPremise({ ok: true, httpVersion: "1.1", connection: "keep-alive" }), true, "#T3 显式 keep-alive 头推断成立");
  assert.equal(pooledHeaderPremise({ ok: true, httpVersion: "1.0", connection: "keep-alive" }), true, "#T3 1.0+显式 keep-alive 头推断成立（对称）");
  assert.equal(pooledHeaderPremise({ ok: true, httpVersion: "1.0" }), false, "#T3 1.0 缺头不成立");
  assert.equal(pooledHeaderPremise({ ok: true, httpVersion: "1.1" }), true, "#T3 1.1 缺头成立");
  assert.equal(pooledHeaderPremise({ ok: false, error: "x" }), false, "#T3 请求失败不成立");
});

// T4 真 server 实跑（本地 node:http，自定界 + Connection: keep-alive）
await section("T4", async () => {
  const srv = createServer((req, res) => {
    res.writeHead(200, { "Content-Type": "text/plain", "Content-Length": "2", Connection: "keep-alive" });
    res.end("ok");
  });
  // fix-r4（sibling#1 / concurrency#1）：listen 挂 error 监听——端口被占（EADDRINUSE，
  // 如并发跑 p15）时 reject 进本 section 的受控 FAIL（failures 汇总、退出码 1），
  // 不再是未捕获异常/顶层 await 未决、汇总块永不执行（守门尺子假 FAIL / 无受控结论）。
  await new Promise((res, rej) => {
    srv.once("error", (e) => rej(e));
    srv.listen(PORT_MT, "127.0.0.1", res);
  });
  let ka;
  try {
    ka = await keepAliveProbe(PORT_MT);
    assert.equal(ka.freeSocketKeys().length, 0, "#T4 请求前池空");
    const once1 = await ka.once("/health");
    assert.equal(once1.ok, true, "#T4 once 应成功");
    assert.equal(once1.premise, true, "#T4 once 应带回 premise=true（单源判定）");
    assert.equal(once1.reusedSocket, false, "#T4 首请求不应是复用");
    assert.equal(once1.premiseReason, "connection-keep-alive", "#T4 premiseReason 应为 connection-keep-alive");
    assert.equal(pooledHeaderPremise(once1), once1.premise, "#T4 头推断与 once.premise 同源同值");
    const keys = ka.freeSocketKeys();
    assert.equal(keys.length, 1, "#T4 入池后 origin 键恰 1 个");
    assert.ok(keys[0].startsWith(`127.0.0.1:${PORT_MT}:`), "#T4 键形如 host:port:localPort（origin 键）");
    assert.equal(ka.freeSocketCount(), 1, "#T4 close 前池占用数为 1");
    const once2 = await ka.once("/health");
    assert.equal(once2.ok, true, "#T4 第二次请求成功");
    assert.equal(once2.reusedSocket, true, "#T4 第二次请求应复用池内 socket");
    // destroy() 异步摘键（socket 'close' 后才删 freeSockets 条目）——有界轮询（≤1s，20ms 步）
    ka.destroy();
    const pollStart = Date.now();
    while (ka.freeSocketKeys().length > 0 && Date.now() - pollStart < 1000) {
      await new Promise((res) => setTimeout(res, 20));
    }
    assert.equal(ka.freeSocketKeys().length, 0, "#T4 destroy 后池空");
  } finally {
    // fix-r4：listen 失败（EADDRINUSE reject）时 srv.listening=false——不 close
    // （对未 listen 的 server close 回调带 ERR_SERVER_NOT_RUNNING，徒增噪声）。
    // fix-r7（DS r6 concurrency#2 处置）：失败路径也要有界收场——closeAllConnections
    // 须先于 await close（keep-alive 空闲连接会让 close 回调永不落定），agent 池
    // 一并兜底销毁（try 正常尾部的 destroy 重复调用是幂等 no-op）。
    if (ka) ka.destroy();
    srv.closeAllConnections?.();
    if (srv.listening) {
      await new Promise((res) => srv.close(() => res()));
    }
  }
});

// T5 窗分区（sawCleanFin 上界排他 / eventsAfterWrite 同毫秒归写后窗）
await section("T5", () => {
  const ev = [
    { event: "end(FIN/EOF)", at: 100 },
    { event: "error", at: 150 },
    { event: "end(FIN/EOF)", at: 200 },
  ];
  assert.equal(sawCleanFin(ev, 50, 150), true, "#T5 [50,150) 窗含 at=100 的干净 FIN");
  assert.equal(sawCleanFin(ev, 50, 100), false, "#T5 上界排他：at=100 不入 [50,100)");
  assert.equal(sawCleanFin(ev, 101), true, "#T5 无上界窗含后续 FIN");
  assert.equal(sawCleanFin(ev, 300), false, "#T5 窗外不判");
  const aw = eventsAfterWrite(ev, 100);
  assert.equal(aw.length, 3, "#T5 写后窗含 at>=writeAt 全部（含同毫秒 at=100）");
  assert.equal(aw[0].relToWrite, 0, "#T5 relToWrite=0（同毫秒归写后窗）");
  assert.equal(aw[2].relToWrite, 100, "#T5 relToWrite 保留差值");
});

// T6 silentHang——fix-r3 P1 簇：flush 守门（flushed:false / pending 不得判「静默悬挂」）
await section("T6", () => {
  assert.equal(silentHang({ writeOk: true, flush: { flushed: true } }, []), true, "#T6 写成功（writeOk+flushed）+无事件=静默悬挂");
  assert.equal(silentHang({ writeOk: true, flush: { flushed: true } }, [{ event: "data", at: 1 }]), false, "#T6 写后有回包非静默");
  assert.equal(silentHang({ writeOk: true, flush: { flushed: true } }, [{ event: "error", at: 1 }]), false, "#T6 写后有 error 非静默");
  assert.equal(silentHang({ writeOk: true, flush: { flushed: true } }, [{ event: "close", hadError: true, at: 1 }]), false, "#T6 写后异常 close 非静默");
  assert.equal(silentHang({ writeOk: true, flush: { flushed: true } }, [{ event: "close", hadError: false, at: 1 }]), true, "#T6 干净 close 不否定静默");
  assert.equal(silentHang({ writeOk: false }, []), false, "#T6 写同步失败即非静默");
  // fix-r3 评审 P1 簇两支负控：写回调失败 / 1.5s 未回调，各自不得判「静默悬挂」
  assert.equal(silentHang({ writeOk: true, flush: { flushed: false, code: "ERR_STREAM_DESTROYED" } }, []), false, "#T6 写回调报错（flushed:false）不得判静默悬挂（写后窗空、error 全落写前窗的实测形状）");
  assert.equal(silentHang({ writeOk: true, flush: { pending: true } }, []), false, "#T6 写回调 1.5s 未触发（pending）不得判静默悬挂");
  assert.equal(silentHang({ writeOk: true }, []), false, "#T6 flush 未落（写同步抛形状）不得判静默悬挂");
});

// T6b isAbnormalEvent（fix-r3 共用谓词：error ∥ close(hadError)）
await section("T6b", () => {
  assert.equal(isAbnormalEvent({ event: "error", code: "EPIPE" }), true, "#T6b error 事件为异常");
  assert.equal(isAbnormalEvent({ event: "close", hadError: true }), true, "#T6b close(hadError=true) 为异常");
  assert.equal(isAbnormalEvent({ event: "close", hadError: false }), false, "#T6b 干净 close 非异常");
  assert.equal(isAbnormalEvent({ event: "data", bytes: 3 }), false, "#T6b data 非异常");
  assert.equal(isAbnormalEvent({ event: "end(FIN/EOF)" }), false, "#T6b FIN 非异常");
});

// T7 rawReuseNote 五分叉
await section("T7", () => {
  const n1 = rawReuseNote({ rawPremise: true, finBeforeWrite: true, eventsAfterWrite: [] });
  assert.ok(n1.startsWith("裸层复用（allowHalfOpen）"), "#T7 前提+写前 FIN → 裸层复用观测");
  const n2 = rawReuseNote({ rawPremise: true, finBeforeWrite: false, eventsAfterWrite: [{ event: "end(FIN/EOF)", at: 2 }] });
  assert.ok(n2.includes("FIN 在写后观测窗内到达"), "#T7 FIN 迟到分叉随实测");
  const n3 = rawReuseNote({ rawPremise: true, finBeforeWrite: false, eventsAfterWrite: [{ event: "error", at: 2 }] });
  assert.ok(n3.includes("error→close 异常拆链"), "#T7 异常拆链分叉随实测");
  const n3b = rawReuseNote({ rawPremise: true, finBeforeWrite: false, eventsAfterWrite: [{ event: "close", hadError: true, at: 2 }] });
  assert.ok(n3b.includes("error→close 异常拆链"), "#T7 只余 close(hadError) 也判异常拆链（共用谓词）");
  const n4 = rawReuseNote({ rawPremise: true, finBeforeWrite: false, eventsAfterWrite: [] });
  assert.ok(n4.includes("静默"), "#T7 静默分叉随实测");
  const n5 = rawReuseNote({ rawPremise: false, finBeforeWrite: true, eventsAfterWrite: [] });
  assert.ok(n5.startsWith("前提未成立"), "#T7 前提未成立按现状记录");
});

// T8 reuseSilentHangClause 四格（fix-r3 措辞含 flush 未确认分叉）
await section("T8", () => {
  assert.ok(reuseSilentHangClause(true, true, true).includes("为静默悬挂"), "#T8 静默句产出");
  assert.ok(reuseSilentHangClause(true, true, true).includes("写回调 flushed"), "#T8 静默句注明 flushed 守门");
  assert.ok(reuseSilentHangClause(true, true, false).includes("非静默悬挂"), "#T8 非静默句产出");
  assert.ok(reuseSilentHangClause(true, true, false).includes("写回调未确认"), "#T8 非静默句含 flush pending/失败分叉");
  assert.ok(reuseSilentHangClause(false, true, true).includes("不引用其结论"), "#T8 前提未成立不引用");
  assert.ok(reuseSilentHangClause(true, false, true).includes("不引用其结论"), "#T8 写前无 FIN 不引用");
});

// T9 partBNote（fix-r3：rst 分支异常判定含 close(hadError)——共用谓词）
await section("T9", () => {
  const rst = partBNote("rst", [
    { event: "connect(handshake-ok)", at: 1 }, { event: "error", code: "ECONNRESET", at: 2 },
  ]);
  assert.equal(rst.sawConnect, true, "#T9 rst sawConnect");
  assert.equal(rst.sawError, true, "#T9 rst sawError");
  assert.equal(rst.sawAbnormal, true, "#T9 rst sawAbnormal");
  assert.ok(rst.note.includes("收到 RST"), "#T9 rst RST 句");
  // fix-r3 sibling#5：只观测到 close(hadError=true) 而 error 事件未入 rawEvents（采样
  // 时序错过）时，rst 分支不得降级为「RST 未到达」——与 rawReuseNote 同构判定。
  const rstCloseOnly = partBNote("rst", [
    { event: "connect(handshake-ok)", at: 1 }, { event: "close", hadError: true, at: 2 },
  ]);
  assert.ok(rstCloseOnly.note.includes("收到 RST"), "#T9 rst 只余 close(hadError) 仍判 RST（共用谓词）");
  assert.equal(rstCloseOnly.sawError, false, "#T9 rst close-only sawError=false（原值分立）");
  assert.equal(rstCloseOnly.sawAbnormal, true, "#T9 rst close-only sawAbnormal=true");
  const eof = partBNote("eof", [
    { event: "connect(handshake-ok)", at: 1 }, { event: "end(FIN/EOF)", at: 2 },
  ]);
  assert.ok(eof.note.includes("收到 EOF"), "#T9 eof EOF 句");
  assert.equal(eof.sawEnd, true, "#T9 eof sawEnd");
  // fix-r4（sibling#2 / boundary#3）：end + close(hadError=true) 而 error 事件未入
  // rawEvents（采样时序错过）——异常拆链形状不得判「无错误」的干净 EOF（与 rst 分支
  // 「只余 close(hadError) 仍判 RST」同构），单列降级句、原值分立断言。
  const eofCloseOnly = partBNote("eof", [
    { event: "connect(handshake-ok)", at: 1 }, { event: "end(FIN/EOF)", at: 2 }, { event: "close", hadError: true, at: 3 },
  ]);
  assert.ok(!eofCloseOnly.note.includes("无错误"), "#T9 eof end+close(hadError) 不得判「无错误」的干净 EOF");
  assert.ok(!eofCloseOnly.note.includes("收到 EOF（end 事件"), "#T9 eof end+close(hadError) 不得落干净 EOF 结论句");
  assert.ok(eofCloseOnly.note.includes("不作 EOF 观测面证据"), "#T9 eof end+close(hadError) 落单列降级句");
  assert.equal(eofCloseOnly.sawEnd, true, "#T9 eof close-only sawEnd（原值分立）");
  assert.equal(eofCloseOnly.sawError, false, "#T9 eof close-only sawError=false（原值分立）");
  assert.equal(eofCloseOnly.sawAbnormal, true, "#T9 eof close-only sawAbnormal=true");
  const noConn = partBNote("rst", [{ event: "error", code: "ECONNRESET", at: 1 }]);
  assert.ok(noConn.note.includes("未观测到 connect"), "#T9 握手前 error 如实降级");
  const eofErr = partBNote("eof", [
    { event: "connect(handshake-ok)", at: 1 }, { event: "error", code: "ECONNRESET", at: 2 },
  ]);
  assert.ok(eofErr.note.includes("error 而非干净 EOF"), "#T9 eof error 降级");
  const rstNoErr = partBNote("rst", [{ event: "connect(handshake-ok)", at: 1 }]);
  assert.ok(rstNoErr.note.includes("未收到 error"), "#T9 rst 无异常降级");
  const eofNoEnd = partBNote("eof", [{ event: "connect(handshake-ok)", at: 1 }]);
  assert.ok(eofNoEnd.note.includes("未观测到 EOF"), "#T9 eof 无 EOF 降级");
});

// T10 pooledSkipReason（fix-r3 boundary#3：头推断成立时不断言「Connection 非 keep-alive」）
await section("T10", () => {
  assert.equal(pooledSkipReason(P2.failed, undefined, derivePooledPremise(P2.failed, undefined)), "pooled1 failed", "#T10 pooled1 failed 分支");
  assert.ok(pooledSkipReason(P2.okKa, undefined, derivePooledPremise(P2.okKa, undefined)).includes("不可观测"), "#T10 freeSockets 不可观测分支");
  const hdrTrue = pooledSkipReason(P2.okKa, [], derivePooledPremise(P2.okKa, []));
  assert.ok(hdrTrue.includes("头推断成立"), "#T10 头推断成立分支（freeSockets 空）");
  assert.ok(hdrTrue.includes("无自定界"), "#T10 成因指向响应无自定界");
  assert.ok(!hdrTrue.includes("Connection 非 keep-alive"), "#T10 头推断=keep-alive 时不得断言 Connection 非 keep-alive");
  const hdrFalse = pooledSkipReason(P2.okClose, [], derivePooledPremise(P2.okClose, []));
  assert.ok(hdrFalse.includes("头推断亦不成立"), "#T10 头推断不成立分支");
  assert.ok(hdrFalse.includes("非 keep-alive"), "#T10 头推断不成立时保留 Connection 非 keep-alive 文案");
});

// T11 agentReuseNote（fix-r3 sibling#6/boundary#2：freeKeysBeforeReuse 按长度分叉）
await section("T11", () => {
  const common = {
    reuseSkipReason: "复用直测前提未成立（见 A/SKIP-agent-reuse）",
    rawPremise: true,
    finBeforeWrite: true,
    silentHangDerived: true,
  };
  const skipped = agentReuseNote({ agentReuse: { skipped: true, reason: common.reuseSkipReason }, freeKeysBeforeReuse: undefined, ...common });
  assert.ok(skipped.includes(`复用直测跳过（${common.reuseSkipReason}）`), "#T11 skipped 分支带 reuseSkipReason");
  const refusedEmpty = agentReuseNote({
    agentReuse: { ok: false, code: "ECONNREFUSED", reusedSocket: false },
    freeKeysBeforeReuse: [],
    ...common,
  });
  assert.ok(refusedEmpty.includes("connect 相位错误"), "#T11 ECONNREFUSED+池空 → connect 相位解释");
  assert.ok(refusedEmpty.includes("freeKeysBeforeReuse 空为直接观测"), "#T11 池空半句成立");
  assert.ok(refusedEmpty.includes("为静默悬挂"), "#T11 静默对照句并入");
  const refusedNonEmpty = agentReuseNote({
    agentReuse: { ok: false, code: "ECONNREFUSED", reusedSocket: false },
    freeKeysBeforeReuse: ["127.0.0.1:39896:50000"],
    ...common,
  });
  assert.ok(refusedNonEmpty.includes("非空"), "#T11 池未排空时如实写非空");
  assert.ok(refusedNonEmpty.includes("池未排空"), "#T11 池未排空降级句");
  assert.ok(!refusedNonEmpty.includes("空为直接观测"), "#T11 池非空时不得写「空为直接观测」");
  assert.ok(refusedNonEmpty.includes("为静默悬挂"), "#T11 池未排空降级句的静默对照仍按 silentHangDerived 推导（=true 时产出正向句）");
  const unexpected = agentReuseNote({
    agentReuse: { ok: false, code: "EPIPE", reusedSocket: false },
    freeKeysBeforeReuse: [],
    ...common,
  });
  assert.ok(unexpected.includes("非预期形状"), "#T11 非 ECONNREFUSED 形状不套 connect 相位解释");
});

// T12 import 无副作用（fix-r3 sibling#7/boundary#6/simplify#3）：子进程全新模块图 import
// 探针本体（A85_OUT 指向专用 scratch），import 后 scratch 不得被创建——路径/Recorder
// 惰性初始化于 main()，import 不建目录、不落盘。
await section("T12", async () => {
  const scratch = resolve(tmpdir(), `p16-gate-mt-scratch-${process.pid}-${Date.now()}`);
  const p16Url = pathToFileURL(resolve(dirname(fileURLToPath(import.meta.url)), "p16_stop_acceptance.mjs")).href;
  const child = spawnSync(
    process.execPath,
    ["--input-type=module", "-e", `import(${JSON.stringify(p16Url)}).then(() => console.log("IMPORTED"), (e) => { console.error("IMPORT-FAIL", e); process.exit(1); })`],
    { env: { ...process.env, A85_OUT: scratch }, timeout: 15000, encoding: "utf8" },
  );
  assert.equal(child.status, 0, `#T12 子进程 import 探针须成功（status=${child.status} ${child.stderr}）`);
  assert.ok((child.stdout ?? "").includes("IMPORTED"), "#T12 子进程 import 完成信号");
  assert.equal(existsSync(scratch), false, "#T12 import 不得创建 A85_OUT 目录（惰性初始化，无副作用）");
});

// T13 守卫纯函数三分支（fix-r4 评审 boundary#1）：isMainEntry(main, argv1Href, moduleUrlHref)
// ——main=true 命中 / main 不提供（22.0–22.16 段形状）但 argv 归一后命中 / 两者皆不命中
// （期望不跑 main）/ 现代 Node 被导入（main===false，期望不命中且宿主安静继续）。
await section("T13", () => {
  assert.equal(isMainEntry(true, undefined, "file:///x/p16.mjs"), true, "#T13 main=true 命中（argv 缺失不影响）");
  assert.equal(isMainEntry(true, "file:///a.mjs", "file:///x/p16.mjs"), true, "#T13 main=true 时 argv 不匹配仍命中");
  assert.equal(isMainEntry(undefined, "file:///x/p16.mjs", "file:///x/p16.mjs"), true, "#T13 main 不提供但 argv 归一后命中（22.0–22.16 直接执行形状）");
  assert.equal(isMainEntry(undefined, "file:///other.mjs", "file:///x/p16.mjs"), false, "#T13 两者皆不命中（被 import：argv[1] 是别的文件）→ 不跑 main");
  assert.equal(isMainEntry(undefined, undefined, "file:///x/p16.mjs"), false, "#T13 main 不提供且 argv 缺失（-e 宿主形状）→ 不跑 main");
  assert.equal(isMainEntry(false, "file:///other.mjs", "file:///x/p16.mjs"), false, "#T13 现代 Node 被导入（main===false）→ 不跑 main（本微测自身即此形状）");
});

// T14 直接执行正控（fix-r4 评审 boundary#1）：node 直接执行探针须跑 main——守卫坏了
// （判成非 main）时零输出 + 静默 exit 0/exit 3，本格必红。用不存在的 --base 让 main()
// 快速受控失败（P16 FAILED + exit 1 + rec.dump 兜底落 JSON）：机器无关、秒级、不依赖
// TS 基线 dist；全量输出正控由轮次验证腿的真实探针直跑承担（P16 DONE + 完整 JSON）。
// fix-r6（评审 concurrency#3，P2a）：scratch 临时目录 finally rmSync 清场（对齐 T15
// :424-434/:445-452）+ 清场后断言（删 rmSync 变异在此红——清场判杀绑定点）。
await section("T14", async () => {
  const scratch = resolve(tmpdir(), `p16-direct-mt-${process.pid}-${Date.now()}`);
  try {
    const probePath = fileURLToPath(new URL("./p16_stop_acceptance.mjs", import.meta.url));
    const child = spawnSync(
      process.execPath,
      [probePath, "--base", resolve(scratch, "no-such-base"), "--out", resolve(scratch, "out")],
      { timeout: 30_000, encoding: "utf8" },
    );
    assert.notEqual(child.status, 0, `#T14 直接执行不得静默 exit 0（status=${child.status}，守卫误判为非 main 的假通过面）`);
    assert.ok((child.stdout ?? "").includes("P16 FAILED"), `#T14 直接执行须跑 main（P16 FAILED 信号；stdout=${JSON.stringify(child.stdout ?? "")}）`);
    assert.equal(existsSync(resolve(scratch, "out", "p16_stop_acceptance.json")), true, "#T14 main() 失败路径亦落 JSON（rec.dump 兜底）");
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
  assert.equal(existsSync(scratch), false, "#T14 直跑负控格 scratch 临时目录须清场（rmSync finally，fix-r6 concurrency#3）");
});

// T15 entryHrefCandidates 导出纯函数直调格（fix-r5 评审 boundary#1 P1；fix-r6 评审
// sibling#2 声明订正）：直调导出函数、用真实文件系统形状（symlink/空格路径/目录/不
// 存在路径）实跑归一——T13 只测纯函数手写 href、T14 在 v22.22.0 命中
// import.meta.main===true 首分支（argv 实参不参与比较），**函数体内**变异（返回字段
// 互换 / 去 realpathSync 归一 / pathToFileURL 误用）在 fix-r5 前在任何 Node 版本上都
// 不被微测判杀、由本格判杀。本格不经 p16_stop_acceptance.mjs 模块作用域的真实调用点
// （实参由本微测构造）——调用点接线零运行时覆盖，登记待办（coord 跟进单）。断言全部
// 走真实 realpathSync/pathToFileURL 路径，不手写 href；临时件（symlink/目录）finally
// 清场，不落仓库。全同步 fs 操作，无子进程。
await section("T15", async () => {
  const probePath = fileURLToPath(new URL("./p16_stop_acceptance.mjs", import.meta.url));
  const probeUrl = pathToFileURL(probePath).href;
  // ① symlink 指到本探针（「argv[1] 传 symlink 路径、两 href 必须相等」的生产形状）：
  //    去 realpathSync 归一变异 → argv1Href 停在 symlink 自身 href ≠ selfHref → 红
  const alias = resolve(tmpdir(), `p16-mt-alias-${process.pid}-${Date.now()}.mjs`);
  symlinkSync(probePath, alias);
  try {
    const h = entryHrefCandidates(alias, probeUrl);
    assert.equal(h.argv1Href, h.selfHref, "#T15a symlink 入口两 href 必须 realpath 归一后相等");
    assert.equal(fileURLToPath(h.argv1Href), realpathSync(probePath), "#T15a argv1Href 回解到探针真实路径");
  } finally {
    rmSync(alias);
  }
  // ② 真实路径含空格形状：pathToFileURL 须 URL 编码（%20）——手工拼 href 变异在此红
  const spaceDir = resolve(tmpdir(), `p16 mt space ${process.pid}-${Date.now()}`);
  mkdirSync(spaceDir, { recursive: true });
  const target = resolve(spaceDir, "target.mjs");
  writeFileSync(target, "export const t15 = 1;\n");
  const alias2 = resolve(spaceDir, "alias.mjs");
  symlinkSync(target, alias2);
  try {
    const h = entryHrefCandidates(alias2, pathToFileURL(target).href);
    assert.equal(h.argv1Href, h.selfHref, "#T15b 空格路径 symlink 两 href 归一后相等");
    assert.ok(h.argv1Href.includes("%20"), "#T15b href 须 URL 编码空格（%20）——手工拼 href 变异在此红");
    assert.equal(fileURLToPath(h.argv1Href), realpathSync(target), "#T15b argv1Href 回解到目标真实路径");
    // ③ argv[1] 为目录：realpathSync 对目录成功归一（实现语义实测口径）——断言其 href
    const hd = entryHrefCandidates(spaceDir, probeUrl);
    assert.notEqual(hd.argv1Href, undefined, "#T15c argv[1] 为目录时 realpathSync 成功归一（非 undefined 口径）");
    assert.equal(fileURLToPath(hd.argv1Href), realpathSync(spaceDir), "#T15c 目录 href 回解到目录真实路径");
    assert.ok(hd.argv1Href.includes("%20"), "#T15c 目录 href 亦须 URL 编码空格");
  } finally {
    rmSync(spaceDir, { recursive: true, force: true });
  }
  // ④ argv[1] 不存在 / 缺失（`-e` 宿主形状）：argv1Href 为 undefined（不命中兜底、不抛）；
  //    selfHref 恒归一到本模块真实路径——两 href 传反变异下 ④ 的 argv1Href 断言即红
  const missing = resolve(tmpdir(), `p16-no-such-${process.pid}-${Date.now()}`);
  const hm = entryHrefCandidates(missing, probeUrl);
  assert.equal(hm.argv1Href, undefined, "#T15d argv[1] 不存在 → argv1Href undefined（不命中兜底、不抛）");
  assert.equal(fileURLToPath(hm.selfHref), realpathSync(probePath), "#T15d selfHref 归一到本模块真实路径");
  const hu = entryHrefCandidates(undefined, probeUrl);
  assert.equal(hu.argv1Href, undefined, "#T15d argv[1] 缺失（-e 宿主形状）→ argv1Href undefined");
  assert.equal(fileURLToPath(hu.selfHref), realpathSync(probePath), "#T15d selfHref 同上（argv 缺失不影响）");
});

// T16 分支③独立复检直调格（fix-r6 评审 sibling#1/concurrency#1，P1）：isSelfEntryPath
// (argv1, selfPath)——路径级 realpathSync 归一比较（不建 href），判「argv[1] 即本模块
// 文件（直接执行形状）」。本微测自身即「被 import 宿主」形状（argv[1]=本微测文件 ≠
// 探针）——复检在该形状须 false（宿主不被杀的判别面，评审场景）；同 T15 口径：纯函数
// 直调，分支③ else-if 调用点接线不在本格覆盖面、登记待办（coord 跟进单）。全同步
// fs 操作，临时件 finally 清场。
await section("T16", async () => {
  const probePath = fileURLToPath(new URL("./p16_stop_acceptance.mjs", import.meta.url));
  assert.equal(isSelfEntryPath(probePath, probePath), true, "#T16a 直接执行形状：argv[1] 即本模块文件 → true");
  // symlink 入口：realpathSync 归一双侧（与守卫/entryHrefCandidates 同语义）——去归一变异在此红
  const alias = resolve(tmpdir(), `p16-mt-selfalias-${process.pid}-${Date.now()}.mjs`);
  symlinkSync(probePath, alias);
  try {
    assert.equal(isSelfEntryPath(alias, probePath), true, "#T16a symlink 入口 realpath 归一后即本模块文件 → true");
  } finally {
    rmSync(alias);
  }
  // 被 import 宿主形状：argv[1] 是宿主文件（本微测自身路径）→ false（宿主不被杀）
  const hostPath = fileURLToPath(import.meta.url);
  assert.equal(isSelfEntryPath(hostPath, probePath), false, "#T16b 被 import 宿主形状：argv[1] 为宿主文件 → false（不杀宿主）");
  // argv[1] 缺失 / 不可 stat（-e/REPL/已删文件形状）→ false
  assert.equal(isSelfEntryPath(undefined, probePath), false, "#T16c argv[1] 缺失（-e 宿主形状）→ false");
  const missing = resolve(tmpdir(), `p16-no-such-self-${process.pid}-${Date.now()}`);
  assert.equal(isSelfEntryPath(missing, probePath), false, "#T16c argv[1] 不存在（不可 stat）→ false");
});

// fix-r4：正常收尾——watchdog 计时器清除（不拖长正常跑；挂起路径由 watchdog 自身收场）
clearTimeout(watchdogTimer);
if (failures.length > 0) {
  console.error(`p16_gate_microtest FAILED（${failures.length} 处）:`);
  for (const f of failures) console.error(`  ${f}`);
  process.exit(1);
}
console.log("p16_gate_microtest: ALL PASS (T1–T16)");

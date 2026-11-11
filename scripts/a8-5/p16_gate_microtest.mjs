/**
 * P16 守门/note 纯函数的入仓负控微测（#2759 fix-r3，评审 sibling#4：仓内须有
 * import p16_stop_acceptance.mjs 真实代码的负控测试，不复制逻辑）。直调本文件导出的
 * 纯函数 + common 的 keepAliveProbe（本地 node:http server，不依赖 TS 基线 dist）。
 * 每条断言带 #T<N> 专属标记，供变异负控绑定判杀；全部 section 顺序执行、失败不
 * 中止（收集后统一汇总，任一失败退出码 1）。
 * 运行：node scripts/a8-5/p16_gate_microtest.mjs（无参数、无落盘副作用——p16 的
 * 路径/Recorder 惰性初始化于 main()，import 本体无 mkdir/argv 副作用，fix-r3）。
 * 端口纪律：39894（微测专用，避让探针 39896–39898；段 39871–39898）。
 */
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { keepAliveProbe, keepAlivePremise } from "./common.mjs";
import {
  parseRawGate, pooledHeaderPremise, derivePooledPremise,
  sawCleanFin, eventsAfterWrite, silentHang, isAbnormalEvent,
  rawReuseNote, reuseSilentHangClause, partBNote,
  pooledSkipReason, agentReuseNote,
} from "./p16_stop_acceptance.mjs";

const PORT_MT = 39894;
const failures = [];
const section = async (name, fn) => {
  try {
    await fn();
  } catch (e) {
    failures.push(`[${name}] ${e?.message ?? e}`);
  }
};

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
  await new Promise((res) => srv.listen(PORT_MT, "127.0.0.1", res));
  try {
    const ka = await keepAliveProbe(PORT_MT);
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
    await new Promise((res) => srv.close(() => res()));
    srv.closeAllConnections?.();
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
  const p16Url = pathToFileURL(resolve(dirname(new URL(import.meta.url).pathname), "p16_stop_acceptance.mjs")).href;
  const child = spawnSync(
    process.execPath,
    ["--input-type=module", "-e", `import(${JSON.stringify(p16Url)}).then(() => console.log("IMPORTED"), (e) => { console.error("IMPORT-FAIL", e); process.exit(1); })`],
    { env: { ...process.env, A85_OUT: scratch }, timeout: 15000, encoding: "utf8" },
  );
  assert.equal(child.status, 0, `#T12 子进程 import 探针须成功（status=${child.status} ${child.stderr}）`);
  assert.ok((child.stdout ?? "").includes("IMPORTED"), "#T12 子进程 import 完成信号");
  assert.equal(existsSync(scratch), false, "#T12 import 不得创建 A85_OUT 目录（惰性初始化，无副作用）");
});

if (failures.length > 0) {
  console.error(`p16_gate_microtest FAILED（${failures.length} 处）:`);
  for (const f of failures) console.error(`  ${f}`);
  process.exit(1);
}
console.log("p16_gate_microtest: ALL PASS (T1–T12)");

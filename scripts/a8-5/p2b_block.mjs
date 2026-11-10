/**
 * P2b：TS server.close() 到底被什么阻塞。
 * A: 无连接；B: 一条已建立 WS；C: 一条空闲 keep-alive HTTP；D: WS 关闭后。
 */
import { writeSync } from "node:fs";
import {
  makeFakeGateway, makeHome, makeStaticDir, keepAliveProbe, wsConnect, Recorder,
  makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p2b-block"), "p2b_block");
const rec = new Recorder(RAW_DIR);
const say = (s) => writeSync(1, `${s}\n`);

async function freshServer(port) {
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  return startPilotDeckServer({
    gateway: makeFakeGateway(),
    port,
    host: "127.0.0.1",
    token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR),
    pilotHome: makeHome(RAW_DIR, "p2b"),
  });
}

async function timedClose(server, label, holdMs, extra = {}) {
  const t0 = Date.now();
  let settled = false;
  const p = server.close().then(
    () => {
      settled = true;
    },
    () => {
      settled = true;
    },
  );
  await new Promise((r) => setTimeout(r, holdMs));
  // fix-r2（Grok boundary#2 + DS concurrency#2 撞条加固）：skipped 降级行不落
  // settledAfterHoldMs 实测值、返回值同取 null——是否降级由本函数显式决定，
  // 不再依赖调用方以 null 经对象展开顺序覆盖实测值（记录与控制流同源）。
  const settledAfterHoldMs = extra.skipped === true ? null : settled;
  rec.log(label, { ...extra, holdMs, settledAfterHoldMs });
  return { p, settledAfterHoldMs };
}

async function main() {
  // A: 无连接
  {
    const s = await freshServer(39874);
    const { settledAfterHoldMs } = await timedClose(s, "A:no-connections", 1500);
    say(`A settled=${settledAfterHoldMs}`);
  }
  // B: 一条已建立 WS
  {
    const s = await freshServer(39875);
    const ws = await wsConnect(BASE, 39875, TOKEN);
    await ws.hello();
    const { p, settledAfterHoldMs } = await timedClose(s, "B:one-ws-open", 2000);
    say(`B settled=${settledAfterHoldMs}`);
    if (!settledAfterHoldMs) {
      ws.close(1000, "probe-release");
      const t0 = Date.now();
      await p;
      rec.log("B:after-ws-close", { resolvedMsAfterClientClose: Date.now() - t0 });
      say(`B after-client-close resolved in ${Date.now() - t0}ms`);
    }
  }
  // C: 空闲 keep-alive HTTP
  {
    const s = await freshServer(39876);
    const ka = await keepAliveProbe(39876);
    // #2756（升 P1，同 p16 #2662 守门形状）：入池前提守门——首次请求失败或响应
    // Connection 非 keep-alive（缺头按完整 HTTP/1.1 隐式，RFC 7230 §6.3）时 agent
    // 池内无 idle socket，本格退化成「无连接」状态 A，settled 仍会 true——仍按
    // A14②「空闲 keep-alive settle」引用即假证据，故记 SKIP 行、结论行降级。
    const first = await ka.once("/health");
    const premise = first.premise === true;
    if (!premise) {
      rec.log("C/SKIP-idle-keepalive", {
        reason: first.premiseReason,
        first,
        note: "入池前提未成立（首次请求失败或响应 Connection 非 keep-alive，agent 池内无 idle socket）——C 格退化成无连接状态 A，settled 不作 A14②「空闲 keep-alive settle」对照",
      });
    }
    // #2756 fix-r1（boundary P1 仲裁 + 评审 boundary P2 三态统一）：入池前提 =
    // 首请求入池 + close 前最后一刻池仍占用（服务端空闲收尾窗实测 ~5.5–6s；
    // server.close() 本身 ~11ms 内即 FIN 掉池内 socket，判池须在 close 前，见
    // 轮次证据 exp-close-pool.out）。前提不成立时结论行记 skipped:true、
    // settledAfterHoldMs 置 null——与 p1_p2/p6/p16 三探针三态编码统一
    // （settled 原值不再落「A14② settle」证据位：该降级状态的 settle 属无连接
    // 状态 A，已被 A 格覆盖）。fix-r2（DS concurrency#2 撞条）：复检位置不动
    // （仲裁：timedClose 入口即发起 close，hold 是 close 后的观测窗）；但降级行
    // premise（初始入池前提原值）与 premiseAtClose（close 前池状态）分立，不再
    // 折叠成 premise:false；freeSocketCount 判定与记录同用一份局部快照。
    const freeSocketsBeforeClose = ka.freeSocketCount();
    const premiseAtClose = premise && freeSocketsBeforeClose > 0;
    if (premise && !premiseAtClose) {
      rec.log("C/SKIP-pool-drained-before-reuse", {
        reason: "池在 close 前被服务端空闲超时收尾（首次 /health 入池后、timedClose 前池已空）",
        note: "C 格退化成无连接状态 A（agent 池内已无 idle socket），settled 不作 A14②「空闲 keep-alive settle」对照",
      });
    }
    const { p, settledAfterHoldMs } = premiseAtClose
      ? await timedClose(s, "C:idle-keepalive", 2000, { premise: true, premiseAtClose: true, freeSocketsBeforeClose })
      : await timedClose(s, "C:idle-keepalive", 2000, {
        skipped: true,
        premise,
        premiseAtClose: false,
        freeSocketsBeforeClose,
        reason: premise ? "池在 close 前被服务端空闲超时收尾（见 C/SKIP-pool-drained-before-reuse）" : "入池前提未成立（见 C/SKIP-idle-keepalive）",
      });
    say(premiseAtClose
      ? `C settled=${settledAfterHoldMs}`
      : `C skipped（${premise ? "池在 close 前被收尾，见 C/SKIP-pool-drained-before-reuse" : "入池前提未成立，见 C/SKIP-idle-keepalive"}——不作 A14② 空闲 keep-alive settle 对照）`);
    if (!settledAfterHoldMs) {
      ka.destroy();
      await p;
      say("C resolved after agent destroy");
    }
    ka.destroy();
  }
  rec.dump("p2b_block.json");
  process.exit(0);
}

main().then(
  () => process.exit(0),
  (e) => {
    say(`P2b FAILED: ${e?.stack ?? e}`);
    rec.dump("p2b_block.json");
    process.exit(1);
  },
);

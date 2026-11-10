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
  rec.log(label, { settledAfterHoldMs: settled, holdMs, ...extra });
  return { p, settledAfterHoldMs: settled };
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
    const { p, settledAfterHoldMs } = await timedClose(s, "C:idle-keepalive", 2000, { premise });
    say(`C settled=${settledAfterHoldMs}${premise ? "" : "（前提未成立，见 C/SKIP-idle-keepalive——不作 A14② 空闲 keep-alive settle 对照）"}`);
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

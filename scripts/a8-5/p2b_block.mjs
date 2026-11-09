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

async function timedClose(server, label, holdMs) {
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
  rec.log(label, { settledAfterHoldMs: settled, holdMs });
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
    await ka.once("/health");
    const { p, settledAfterHoldMs } = await timedClose(s, "C:idle-keepalive", 2000);
    say(`C settled=${settledAfterHoldMs}`);
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

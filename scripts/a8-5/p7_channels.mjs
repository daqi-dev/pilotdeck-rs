/**
 * P7：E4 channel × 阶段矩阵。真实 startPilotDeckServer + 受控假 channel。
 * 观察：S0 启动/deliverCronResult/webhook；closing 闸门（hot-start、reconcile）；
 * channel 停机挂起时 listener 仍开 + webhook 已丢 + deliverCronResult 已断；
 * 启动完成竞态（close 后 start 才完成 → 立即 stop + 报错）。
 */
import { writeSync } from "node:fs";
import {
  makeFakeGateway, makeHome, makeStaticDir, httpProbe, wsConnect, Recorder, deferred, sleep,
  makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p7-channels"), "p7_channels");

const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 260)}`);
  rec.log(k, d);
};

function makeChannel(key, hooks) {
  return {
    channelKey: key,
    async start(deps) {
      hooks.started.push(key);
      if (hooks.onStart) await hooks.onStart();
      return {
        stop: async (reason) => { hooks.stops.push({ key, reason }); if (hooks.onStop && reason === "server-shutdown") await hooks.onStop(); },
      };
    },
    async deliverCronResult(delivery) {
      hooks.deliveries.push(delivery);
      return true;
    },
    ...(key === "feishu" ? {
      async handleWebhook(request, response) {
        hooks.webhooks.push(request.url);
        response.writeHead(200, { "content-type": "text/plain" });
        response.end("feishu-probe-ok");
        return true;
      },
    } : {}),
  };
}

async function main() {
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  // ---- 场景 1：正常运行 + closing 闸门 + 停机挂起窗口 ----
  const feishuHooks = { started: [], stops: [], deliveries: [], webhooks: [] };
  const testHooks = { started: [], stops: [], deliveries: [], webhooks: [] };
  const stopHold = deferred(); // test channel 的 stop 挂起
  testHooks.onStop = () => stopHold.promise;
  const hotHooks = { started: [], stops: [], deliveries: [], webhooks: [] };
  hotHooks.onStop = () => stopHold.promise;
  const port = 39884;
  const server = await startPilotDeckServer({
    gateway: makeFakeGateway(), port, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, "p7-channels"),
    channels: [makeChannel("feishu", feishuHooks), makeChannel("test", testHooks)],
  });
  await sleep(300); // 后台启动完成
  log("S0/started", { feishu: feishuHooks.started, test: testHooks.started });
  log("S0/deliverCronResult", { delivered: await server.deliverCronResult({ channelKey: "feishu", taskId: "t1", runId: "r1" }) });
  log("S0/feishu-webhook", await httpProbe(port, "/feishu/webhook", { method: "POST" }));
  log("S0/hot-start-ok", await server.hotStartChannel(makeChannel("test", hotHooks)).then(() => ({ ok: true }), (e) => ({ error: e.message })));

  // close()：test channel 停机挂起 → closing=true、activeFeishu 清空、listener 仍开
  const closePromise = server.close();
  await sleep(200);
  log("closing-window/health", await httpProbe(port, "/health"));
  log("closing-window/feishu-webhook", await httpProbe(port, "/feishu/webhook", { method: "POST" }));
  log("closing-window/hot-start", await server.hotStartChannel(makeChannel("test", { started: [], stops: [], deliveries: [], webhooks: [] })).then(() => ({ ok: true }), (e) => ({ error: e.message })));
  log("closing-window/reconcile", await server.reconcileChannels({ channels: [], managedChannelKeys: [] }).then(() => ({ ok: true }), (e) => ({ error: e.message })));
  log("closing-window/deliverCronResult", { delivered: await server.deliverCronResult({ channelKey: "feishu", taskId: "t2", runId: "r2" }) });
  let wsClosingClient;
  const wsClosing = await wsConnect(BASE, port, TOKEN).then(
    (c) => {
      wsClosingClient = c;
      return c.hello();
    },
    (e) => ({ connectError: e.message }),
  );
  log("closing-window/new-ws-hello", wsClosing);

  stopHold.resolve(); // 放行 channel 停机 → closeServer 继续（listener 关）
  await sleep(100);
  log("close-blocked-by-new-ws", { note: "关闭窗口内新开的连接同样阻塞 server.close（与 p2b 同因）" });
  wsClosingClient?.close(1000, "probe-release");
  const t0 = Date.now();
  await closePromise;
  log("close-settled", { ms: Date.now() - t0, stops: [...feishuHooks.stops, ...testHooks.stops] });
  log("after-close/hot-start", await server.hotStartChannel(makeChannel("test", { started: [], stops: [], deliveries: [], webhooks: [] })).then(() => ({ ok: true }), (e) => ({ error: e.message })));

  // ---- 场景 2：启动完成竞态（close 之后 start 才完成） ----
  const port2 = 39885;
  const slowHooks = { started: [], stops: [], deliveries: [], webhooks: [] };
  const startHold = deferred();
  slowHooks.onStart = () => startHold.promise;
  const server2 = await startPilotDeckServer({
    gateway: makeFakeGateway(), port: port2, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, "p7-race"),
    channels: [makeChannel("test", slowHooks)],
  });
  const close2 = server2.close(); // start 还挂着
  await sleep(200);
  startHold.resolve(); // start 在 closing 之后完成
  await sleep(300);
  log("race/started", { started: slowHooks.started });
  log("race/stops", { stops: slowHooks.stops });
  await close2;
  log("race/close-settled", { ok: true });

  rec.dump("p7_channels.json");
  say("P7 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P7 FAILED: ${e?.stack ?? e}`);
  rec.dump("p7_channels.json");
  process.exit(1);
});

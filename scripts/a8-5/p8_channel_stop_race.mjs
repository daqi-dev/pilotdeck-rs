/**
 * P8：#2251 条目 2 —— closeServer（SIGTERM 路径 step2）与 channel stop 的两个在飞格。
 * 场景 A（hot-reload stop 在飞 × closeServer 交错）：hotStartChannel 触发 replaceChannel，
 *   stopRunningChannel("hot-reload") 已 claim handle（从 runningHandles 摘除）且挂起；
 *   此时 close() 快照 runningHandles —— 被 claim 的 key 不在快照内，其 stop 不被 join，
 *   close 先 settle（ordering 违例）；随后放行（成功）：旧 channel 从未收到 server-shutdown stop，
 *   替换 channel 立即被 startAndTrack 的 closing 闸门 stop 掉并抛错。
 * 场景 B（channel stop 失败）：reason=server-shutdown 下 stop reject → stopRunningChannel
 *   的 catch 在 closing=true 之后把 handle/channel 回填 map（deliverCronResult 复投成功为
 *   可观察证据）；closeServer 聚合抛出；二次 close() 复用同一 rejected promise、不重停。
 */
import { writeSync } from "node:fs";
import {
  makeFakeGateway, makeHome, makeStaticDir, httpProbe, tcpRefused, Recorder, deferred, sleep,
  makePaths, parseProbeArgs, TOKEN,
} from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p8-channel-stop-race"), "p8_channel_stop_race");

const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 300)}`);
  rec.log(k, d);
};

function makeChannel(key, hooks, behavior = {}) {
  return {
    channelKey: key,
    async start(deps) {
      hooks.started.push(key);
      if (behavior.onStartHold) await behavior.onStartHold.promise;
      return {
        async stop(reason) {
          hooks.stops.push({ key, reason });
          if (behavior.onStopHold && reason === behavior.holdReason) await behavior.onStopHold.promise;
          if (behavior.onStopReject && reason === behavior.rejectReason) {
            throw new Error(`inject: ${key} stop failed (${reason})`);
          }
        },
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

async function scenarioA() {
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const port = 39886;
  const feishuHooks = { started: [], stops: [], deliveries: [], webhooks: [] };
  const testHooks = { started: [], stops: [], deliveries: [], webhooks: [] };
  const replaceHooks = { started: [], stops: [], deliveries: [], webhooks: [] };
  const stopHold = deferred(); // hot-reload 的 stop 挂起
  const server = await startPilotDeckServer({
    gateway: makeFakeGateway(), port, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, "p8-a"),
    channels: [
      makeChannel("feishu", feishuHooks),
      makeChannel("test", testHooks, { onStopHold: stopHold, holdReason: "hot-reload" }),
    ],
  });
  await sleep(300); // 后台启动完成
  log("A/S0-started", { feishu: feishuHooks.started, test: testHooks.started });

  // hot-reload stop 在飞（handle 已 claim、挂起）
  const hotPromise = server.hotStartChannel(makeChannel("test", replaceHooks));
  await sleep(200);
  log("A/hot-reload-stop-in-flight", { stopsSoFar: testHooks.stops, hotSettled: false });

  // closeServer（SIGTERM 路径 step2）在 stop 在飞时快照 runningHandles
  const t0 = Date.now();
  const closePromise = server.close().then(
    () => ({ settled: true, ms: Date.now() - t0 }),
    (e) => ({ settled: true, rejected: true, name: e?.constructor?.name, message: e?.message, ms: Date.now() - t0 }),
  );
  await sleep(300);
  const closeEarly = await Promise.race([closePromise, sleep(50).then(() => ({ settled: false }))]);
  log("A/close-while-stop-in-flight", { closeEarly, testStops: testHooks.stops, note: "close 不 join 在飞 hot-reload stop" });

  // 放行 hot-reload stop（成功）
  stopHold.resolve();
  const closeResult = await closePromise;
  await sleep(200);
  const hotResult = await hotPromise.then(
    () => ({ settled: true, ok: true }),
    (e) => ({ settled: true, ok: false, error: e?.message }),
  );
  log("A/after-release", {
    closeResult,
    hotResult,
    oldChannelStops: testHooks.stops, // 旧 channel 只有 hot-reload 一次 stop、从未收到 server-shutdown
    replacementStops: replaceHooks.stops, // 替换 channel 立即被 closing 闸门 stop
    replacementStarted: replaceHooks.started,
    tcpRefused: await tcpRefused(port),
    deliverCronResultTest: await server.deliverCronResult({ channelKey: "test", taskId: "t", runId: "r" }),
  });
}

async function scenarioB() {
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const port = 39887;
  const feishuHooks = { started: [], stops: [], deliveries: [], webhooks: [] };
  const testHooks = { started: [], stops: [], deliveries: [], webhooks: [] };
  const server = await startPilotDeckServer({
    gateway: makeFakeGateway(), port, host: "127.0.0.1", token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR), pilotHome: makeHome(RAW_DIR, "p8-b"),
    channels: [
      makeChannel("feishu", feishuHooks),
      makeChannel("test", testHooks, { onStopReject: true, rejectReason: "server-shutdown" }),
    ],
  });
  await sleep(300);
  log("B/S0-started", { feishu: feishuHooks.started, test: testHooks.started });
  log("B/S0-deliverCronResult", { delivered: await server.deliverCronResult({ channelKey: "test", taskId: "t0", runId: "r0" }) });

  // close：test 的 stop（server-shutdown）失败
  const closeResult = await server.close().then(
    () => ({ settled: true, ok: true }),
    (e) => ({
      settled: true,
      ok: false,
      name: e?.constructor?.name,
      message: e?.message,
      errors: e?.errors?.map((x) => String(x?.message ?? x)),
    }),
  );
  log("B/close-result", closeResult);
  log("B/stops", { feishuStops: feishuHooks.stops, testStops: testHooks.stops });
  // 回填可观察证据：失败 stop 后 handle/channel 回到 map → deliverCronResult 仍可投递
  log("B/deliverCronResult-after-failed-stop", {
    delivered: await server.deliverCronResult({ channelKey: "test", taskId: "t1", runId: "r1" }),
    testDeliveries: testHooks.deliveries.length,
  });
  // 二次 close：复用同一（rejected）promise，不重停
  const close2 = await server.close().then(
    () => ({ settled: true, ok: true }),
    (e) => ({ settled: true, ok: false, message: e?.message }),
  );
  log("B/second-close", { close2, testStopsStill: testHooks.stops.length, note: "失败 channel 永不再被停" });
  log("B/tcp", { refused: await tcpRefused(port), health: await httpProbe(port, "/health") });
}

async function main() {
  await scenarioA();
  await scenarioB();
  rec.dump("p8_channel_stop_race.json");
  say("P8 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P8 FAILED: ${e?.stack ?? e}`);
  rec.dump("p8_channel_stop_race.json");
  process.exit(1);
});

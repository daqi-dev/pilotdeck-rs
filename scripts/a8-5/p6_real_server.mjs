/**
 * P6：真实 `pilotdeck server`（dist 构建）端到端：SIGTERM 时序的进程级观察。
 * - 既有 WS 连接在停机/退出时的表现（服务端从不主动发 WS close 帧）
 * - SIGTERM 同一轮并发：既有 WS 请求 / 新 WS / keep-alive HTTP / 新 HTTP
 * - 退出码、端口释放；旧进程退出后同端口新进程可立即接管（新握手前置）
 */
import { spawn } from "node:child_process";
import { writeSync, mkdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { httpProbe, keepAliveProbe, tcpRefused, wsConnect, Recorder, sleep, makePaths, parseProbeArgs, TOKEN } from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p6-real-server"), "p6_real_server");

const say = (s) => writeSync(1, `${s}\n`);
const rec = new Recorder(RAW_DIR);
const log = (k, d) => {
  say(`[${k}] ${JSON.stringify(d).slice(0, 280)}`);
  rec.log(k, d);
};
const port = 39888;

function spawnServer(cwd, env) {
  const out = [];
  const child = spawn(process.execPath, [
    `${BASE}/dist/src/cli/pilotdeck.js`,
    "server",
  ], { cwd, env, stdio: ["ignore", "pipe", "pipe"] });
  const pump = (stream, tag) => stream.on("data", (d) => {
    for (const line of d.toString().split("\n")) {
      if (line.trim()) out.push(`${tag} ${line}`);
    }
  });
  pump(child.stdout, "[out]");
  pump(child.stderr, "[err]");
  return { child, out };
}

async function waitListening(out, timeoutMs = 20000) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutMs) {
    if (out.some((l) => l.includes("PilotDeck server listening"))) return true;
    await sleep(100);
  }
  return false;
}

async function main() {
  const cwd = resolve(RAW_DIR, "homes", "p6-proj");
  mkdirSync(cwd, { recursive: true });
  const home = resolve(RAW_DIR, "homes", "p6-home");
  mkdirSync(home, { recursive: true });
  // 最小合法配置：真实 loadPilotConfig 要求 agent/model 段。
  // provider 指到不可路由端口，本探针不发起任何模型请求。
  writeFileSync(resolve(home, "pilotdeck.yaml"), [
    "schemaVersion: 1",
    "agent:",
    "  model: probe/probe-model",
    "model:",
    "  providers:",
    "    probe:",
    "      protocol: openai",
    "      url: http://127.0.0.1:9/v1",
    "      apiKey: probe-key",
    "      models:",
    "        probe-model: {}",
    "",
  ].join("\n"));
  const env = {
    ...process.env,
    PILOT_HOME: home,
    PILOTDECK_GATEWAY_PORT: String(port),
    PILOTDECK_GATEWAY_TOKEN: TOKEN,
    PILOTDECK_GATEWAY_HOST: "127.0.0.1",
  };

  // ---- 第一轮：既有 WS + 边界并发 ----
  const { child, out } = spawnServer(cwd, env);
  log("listen", { ok: await waitListening(out) });
  log("pre/health", await httpProbe(port, "/health"));

  const ws = await wsConnect(BASE, port, TOKEN);
  log("pre/ws-hello", { type: (await ws.hello())?.type });
  log("pre/ws-describe", { ok: (await ws.request("describe_server"))?.ok });
  const ka = await keepAliveProbe(port);
  // #2756（同 p16 #2662 守门形状）：建池前提守门——首次请求失败或响应 Connection
  // 非 keep-alive（缺头按完整 HTTP/1.1 隐式）时池空，SIGTERM 轮的 keepalive-http
  // 是新连接在途，「既有 keep-alive 连接被拆」语义不成立（matrix.md §4.2 / §5.1
  // 单样本格 n=1 无退路），故记 SKIP 行、该条目跳过。
  const kaFirst = await ka.once("/health");
  const kaPool = kaFirst.premise === true;
  log("pre/ka", kaFirst);
  if (!kaPool) {
    log("ka/SKIP-sigterm-keepalive-http", {
      reason: kaFirst.premiseReason,
      kaFirst,
      note: "建池前提未成立：sigterm-concurrent 的 keepalive-http 跳过（新连接对照已由 new-http 覆盖，不重复产出），「既有 keep-alive 在途被拆」观测不作对照",
    });
  }

  // #2756 fix-r1（boundary P1 仲裁）：SIGTERM 时刻再判一次池占用——S0→SIGTERM
  // 跨过服务端空闲收尾窗（实测 ~5.5–6s）时池已被收空，keepalive-http 是新连接
  // 在途（实测错误串退化为 ECONNREFUSED、reusedSocket:false，与「既有 keep-alive
  // 在途被拆」的 ECONNRESET 语义不同），仍按单样本口径引用即错读证据。判池须在
  // SIGTERM 前（server 收到 SIGTERM 后 close() ~11ms 内即 FIN 掉池内 socket，
  // 见轮次证据 exp-close-pool.out）。
  const kaPoolAtSigterm = kaPool && ka.freeSocketCount() > 0;
  if (kaPool && !kaPoolAtSigterm) {
    log("ka/SKIP-pool-drained-before-reuse", {
      reason: "池在 SIGTERM 前被服务端空闲超时收尾（pre/ka 入池后、SIGTERM 前池已空）",
      note: "sigterm-concurrent 的 keepalive-http 跳过：池空时是新建连在途而非既有 keep-alive 被拆（新连接对照已由 new-http 覆盖），「既有 keep-alive 在途被拆（ECONNRESET 单样本）」观测不作对照",
    });
  }

  // SIGTERM 同一轮并发四个入口请求
  child.kill("SIGTERM");
  const concurrent = await Promise.all([
    ws.request("describe_server", undefined, 4000).then((r) => ({ kind: "existing-ws", ok: r?.ok ?? r })),
    wsConnect(BASE, port, TOKEN).then(
      (c) => c.hello(),
      (e) => ({ kind: "new-ws", error: e.message }),
    ),
    // #2756：前提未成立时跳过（池空时是新建连而非复用在途，证据无效——同 p16 观察 3
    // 形状）；fix-r1 起前提 = kaPoolAtSigterm（入池 + SIGTERM 前池仍占用），once 返回
    // 带 reusedSocket（窗内正常样本应为 true，即真在池内 socket 上在途）。
    // fix-r2（DS sibling#2 + Grok boundary#1 撞条）：SIGTERM 前的复检不能代替
    // 在途判别——复检通过后、请求发出前池仍可能被收掉（server 收到 SIGTERM 后
    // close() ~11ms 内即 FIN 掉池内 socket），此时请求落新连接，错误串退化
    // ECONNREFUSED、reusedSocket:false，与「既有 keep-alive 在途被拆」语义不同。
    // 事后按 reusedSocket 复核：false（复用池在发出前已被收尾）→ 记
    // ka/SKIP-reuse-fell-to-new-conn、该行降级 skipped，与三态编码对齐。
    // fix-r3（DS sibling#2）：降级行保留 once 原字段（ok/error/code/reusedSocket——
    // p16 fix-r1「失败分支保留 {...r}」先例，读 sigterm-concurrent 数组的消费方不
    // 漏判真实挂起/其它失败形状），reason 按 r.ok / r.reusedSocket 实测值推导
    //（reusedSocket 全路径恒布尔：Node v22.22.0 构造器置 false、freeSockets 弹出
    // 复用置 true，error/timeout 路径同布尔，见 fix-r3 exp-reused-socket-semantics）。
    kaPoolAtSigterm
      ? ka.once("/health").then((r) => {
          if (r.reusedSocket === true) return { kind: "keepalive-http", ...r };
          log("ka/SKIP-reuse-fell-to-new-conn", {
            reusedSocket: r.reusedSocket,
            once: r,
            reason: r.ok === true
              ? `SIGTERM 前池占用复检通过，但请求实际落在新连接（reusedSocket:${r.reusedSocket}，复用池在发出前已被收尾）`
              : `SIGTERM 前池占用复检通过，但请求未在池内 socket 上完成（ok:false${r.code ? `，${r.code}` : r.error ? `，${r.error}` : ""}；reusedSocket:${r.reusedSocket}）——成因以 once 原值为准，不按「落新连接」单义解释`,
            note: "keepalive-http 行降级 skipped：该请求不在池内 socket 上在途，「既有 keep-alive 在途被拆（ECONNRESET 单样本）」观测不作对照（新连接对照已由 new-http 覆盖，不重复产出）",
          });
          return {
            kind: "keepalive-http",
            ...r,
            skipped: true,
            reason: r.ok === true
              ? `请求落新连接而非池内 socket 在途（reusedSocket:${r.reusedSocket}，见 ka/SKIP-reuse-fell-to-new-conn）`
              : `请求未在池内 socket 上完成（reusedSocket:${r.reusedSocket}，once 原值随行——见 ka/SKIP-reuse-fell-to-new-conn）`,
          };
        })
      : Promise.resolve({ kind: "keepalive-http", skipped: true, reason: kaPool ? "池在 SIGTERM 前被服务端空闲超时收尾（见 ka/SKIP-pool-drained-before-reuse）" : "建池前提未成立（见 ka/SKIP-sigterm-keepalive-http）" }),
    httpProbe(port, "/health").then((r) => ({ kind: "new-http", ...r })),
  ]);
  log("sigterm-concurrent", concurrent);

  // 既有 WS 连接的最终去向（等服务端退出；上限 15s）
  const t0 = Date.now();
  while (child.exitCode === null && Date.now() - t0 < 15000) await sleep(100);
  log("exit", { exitCode: child.exitCode, signal: child.signalCode, ms: Date.now() - t0, note: "null=进程未退出（server.close 等既有连接）" });
  log("ws-final", { closed: ws.closed, closeEvent: JSON.parse(JSON.stringify(ws.closeEvent ?? null)) });
  log("port-after-exit", { refused: await tcpRefused(port), note: "listener 已释放但旧进程仍活着 → 双 owner 窗口" });
  log("server-output", { lines: out.slice(0, 40) });
  // 双 owner 并存格：旧进程未退出时，新进程能否在同一端口接管？
  if (child.exitCode === null) {
    const takeover = spawnServer(cwd, env);
    const took = await waitListening(takeover.out, 8000);
    const wsTakeover = took ? await wsConnect(BASE, port, TOKEN).then(
      (c) => c.hello(),
      (e) => ({ connectError: e.message }),
    ) : { notListened: true };
    log("concurrent-takeover", { oldProcessAlive: child.exitCode === null, newListened: took, newWsHello: wsTakeover?.type ?? wsTakeover });
    const wsOldStillServing = await ws.request("describe_server", undefined, 3000);
    log("old-still-serving-existing-ws", { ok: wsOldStillServing?.ok ?? wsOldStillServing });
    if (took) {
      takeover.child.kill("SIGKILL");
      const tk = Date.now();
      while (takeover.child.exitCode === null && Date.now() - tk < 5000) await sleep(100);
    }
  }
  // 收尸：挂起的旧进程必须显式杀掉，否则泄漏
  if (child.exitCode === null) {
    child.kill("SIGKILL");
    const tk = Date.now();
    while (child.exitCode === null && Date.now() - tk < 5000) await sleep(100);
    log("round1-sigkill", { exitCode: child.exitCode, signal: child.signalCode });
  }
  ws.close();

  // ---- 第二轮：同端口新进程立即接管（S5：新 owner 放开的前置） ----
  const round2 = spawnServer(cwd, env);
  const ok2 = await waitListening(round2.out);
  log("round2-listen", { ok: ok2 });
  log("round2/health", await httpProbe(port, "/health"));
  const ws2 = await wsConnect(BASE, port, TOKEN);
  log("round2/ws-hello", { type: (await ws2.hello())?.type });
  log("round2/ws-describe", { ok: (await ws2.request("describe_server"))?.ok });
  ws2.close(1000, "probe-done");
  round2.child.kill("SIGTERM");
  const t1 = Date.now();
  while (round2.child.exitCode === null && Date.now() - t1 < 15000) await sleep(100);
  log("round2-exit", { exitCode: round2.child.exitCode, signal: round2.child.signalCode });
  // 没有客户端连接时 SIGTERM 干净退出（对照第一轮：有客户端时是否仍能退出）
  log("round2-output", { lines: round2.out.slice(0, 30) });

  rec.dump("p6_real_server.json");
  say("P6 DONE");
  process.exit(0);
}

main().catch((e) => {
  say(`P6 FAILED: ${e?.stack ?? e}`);
  rec.dump("p6_real_server.json");
  process.exit(1);
});

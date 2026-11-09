/** P1 冒烟：真实 startPilotDeckServer(dist) + 假 Gateway，S0 基线（装置可用性自检）。 */
import { makeFakeGateway, makeHome, makeStaticDir, httpProbe, wsConnect, makePaths, parseProbeArgs, TOKEN } from "./common.mjs";

const { BASE, RAW_DIR } = makePaths(parseProbeArgs("p1-smoke"), "p1_smoke");
const port = 39871;

async function main() {
  const gw = makeFakeGateway();
  const { startPilotDeckServer } = await import(`${BASE}/dist/src/cli/pilotdeckServer.js`);
  const server = await startPilotDeckServer({
    gateway: gw,
    port,
    host: "127.0.0.1",
    token: TOKEN,
    staticAssetsPath: makeStaticDir(RAW_DIR),
    pilotHome: makeHome(RAW_DIR, "p1-smoke"),
  });
  console.log("listening:", server.url, server.wsUrl);
  console.log("health:", JSON.stringify(await httpProbe(port, "/health")));
  console.log("local-token:", JSON.stringify(await httpProbe(port, "/auth/local-token")));
  console.log("api-web:", JSON.stringify(await httpProbe(port, "/api/web/projects")));
  console.log("404:", JSON.stringify(await httpProbe(port, "/definitely-not-here")));
  const ws = await wsConnect(BASE, port, TOKEN);
  console.log("hello:", JSON.stringify(await ws.hello()).slice(0, 200));
  console.log("describe_server:", JSON.stringify(await ws.request("describe_server")).slice(0, 200));
  ws.close();
  await server.close();
  console.log("closed ok");
}

main().then(
  () => process.exit(0),
  (error) => {
    console.error("P1 FAILED:", error);
    process.exit(1);
  },
);

#!/usr/bin/env node
// pilotdeck-agent-loop-sidecar.ts — dynamic factory + AgentLoopSidecarServer.serve()
import { existsSync } from "node:fs";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const sidecarDir = dirname(fileURLToPath(import.meta.url));

async function loadAgentModules() {
  const bundled = join(sidecarDir, "../agent-modules/index.js");
  if (existsSync(bundled)) {
    return import(pathToFileURL(bundled).href);
  }
  const starts = [sidecarDir, process.cwd()];
  for (const start of starts) {
    let dir = start;
    for (;;) {
      const candidate = join(dir, "dist/src/agent/modules/index.js");
      if (existsSync(candidate)) {
        return import(pathToFileURL(candidate).href);
      }
      const parent = dirname(dir);
      if (parent === dir) {
        break;
      }
      dir = parent;
    }
  }
  throw new Error(
    "AgentLoop sidecar modules are unavailable. Install PilotDeck dist assets (dist/src/agent/modules).",
  );
}

const factoryPath = process.env.PILOTDECK_AGENT_LOOP_FACTORY;
let factory;
if (factoryPath) {
  const href = pathToFileURL(
    isAbsolute(factoryPath) ? factoryPath : resolve(process.cwd(), factoryPath),
  ).href;
  const loaded = await import(href);
  factory = loaded.default ?? loaded.createSidecarExecution;
} else {
  const loaded = await import(
    new URL("./pilotdeck-agent-loop-default-factory.js", import.meta.url).href
  );
  factory = loaded.default;
}
if (typeof factory !== "function") {
  throw new Error("Sidecar factory module must export default or createSidecarExecution.");
}

const { AgentLoopSidecarServer, AgentLoopSidecarTcpServer } = await loadAgentModules();
const sidecar = new AgentLoopSidecarServer(factory);
const tcpPort = optionalPort(process.env.PILOTDECK_AGENT_LOOP_TCP_PORT);

async function runSidecar() {
  if (tcpPort === undefined) {
    await sidecar.serve();
  } else {
    const server = new AgentLoopSidecarTcpServer(sidecar);
    await server.listen({
      host: process.env.PILOTDECK_AGENT_LOOP_TCP_HOST?.trim() || "127.0.0.1",
      port: tcpPort,
    });
    await new Promise(() => undefined);
  }
}

runSidecar().catch((error) => {
  console.error(error);
  process.exit(1);
});

function optionalPort(value) {
  if (value === undefined || value.trim() === "") {
    return undefined;
  }
  const port = Number(value);
  if (!Number.isInteger(port) || port < 1 || port > 65_535) {
    throw new Error("PILOTDECK_AGENT_LOOP_TCP_PORT must be an integer between 1 and 65535.");
  }
  return port;
}

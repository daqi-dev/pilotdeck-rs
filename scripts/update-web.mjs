#!/usr/bin/env node
import { createRequire } from "node:module";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

function findWebInstallRoot(start) {
  const extra = process.env.PILOTDECK_WEB_ROOT?.trim();
  if (extra && existsSync(join(extra, "ui/server/services/webUpdateCommand.js"))) {
    return extra;
  }
  for (let current = start; ; current = dirname(current)) {
    if (existsSync(join(current, "ui/server/services/webUpdateCommand.js"))) {
      return current;
    }
    const sibling = join(current, "PilotDeck-standalone");
    if (existsSync(join(sibling, "ui/server/services/webUpdateCommand.js"))) {
      return sibling;
    }
    if (dirname(current) === current) break;
  }
  throw new Error(
    "This installation does not include the Git Web updater. Use application settings or update manually.",
  );
}

const installRoot = findWebInstallRoot(dirname(fileURLToPath(import.meta.url)));
const args = process.argv.slice(2);
try {
  if (args.some((arg) => !["--check", "--restart"].includes(arg))) {
    throw new Error("Usage: update [--check] [--restart]");
  }

  const require = createRequire(pathToFileURL(join(installRoot, "package.json")).href);
  const { register } = require("tsx/esm/api");
  register();
  await import(pathToFileURL(join(installRoot, "ui/server/load-env.js")).href);
  const { installGlobalProxy } = await import(
    pathToFileURL(join(installRoot, "ui/server/utils/proxy.js")).href
  );
  const { runWebUpdateCommand } = await import(
    pathToFileURL(join(installRoot, "ui/server/services/webUpdateCommand.js")).href
  );
  installGlobalProxy();
  process.exitCode = await runWebUpdateCommand({
    checkOnly: args.includes("--check"),
    restart: args.includes("--restart"),
  });
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
}

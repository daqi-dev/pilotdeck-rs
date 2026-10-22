// Run by generate.mts using the fixed baseline's tsx loader. No baseline writes.
import { pathToFileURL } from 'node:url';
import { join } from 'node:path';
const baseline = process.env.PDRS_BASELINE!;
const { runChatSearchCli } = await import(pathToFileURL(join(baseline, 'src/cli/commands/chatSearch.ts')).href);
const { createNodeSessionSearchPort } = await import(pathToFileURL(join(baseline, 'src/session/search/NodeSessionSearchPort.ts')).href);
try { await runChatSearchCli(process.argv.slice(2), createNodeSessionSearchPort()); }
catch (error) { console.error(error instanceof Error ? error.message : String(error)); process.exitCode = 1; }

// Inject only the OS boundary; execute our existing offline callback driver
// against the unchanged fixed TS setup and installed real Weixin SDK.
import {createRequire, syncBuiltinESMExports} from 'node:module';
import {readFileSync, writeFileSync} from 'node:fs';
const fixture = JSON.parse(readFileSync(process.argv[2], 'utf8'));
const os = createRequire(import.meta.url)('node:os');
const calls = [];
os.homedir = () => {
  const value = fixture.home === null ? fixture.account : fixture.home;
  calls.push(value);
  return value;
};
syncBuiltinESMExports();
process.once('exit', () => writeFileSync(process.env.SETUP_HOME_EVENTS,
  JSON.stringify(calls) + '\n', {flag: 'wx'}));
await import('../gateway-setup/weixin-driver.mjs');

// Run the actual CLI entry. Fail before any socket/fetch can leave this child.
import {readFileSync, writeFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {createRequire, syncBuiltinESMExports} from 'node:module';
import {pathToFileURL} from 'node:url';
const [caseFile, source] = process.argv.slice(2);
const fixture = JSON.parse(readFileSync(caseFile, 'utf8'));
const attempts = [];
const blockedDependencyDataFetches = [];
const deny = name => function() { attempts.push(name); throw new Error('offline fixture rejected network: ' + name); };
const require = createRequire(import.meta.url);
const net = require('node:net');
net.Socket.prototype.connect = deny('net.Socket.connect');
net.connect = deny('net.connect');
net.createConnection = deny('net.createConnection');
require('node:tls').connect = deny('tls.connect');
for (const name of ['http', 'https']) {
  const module = require('node:' + name);
  module.request = deny(name + '.request');
  module.get = deny(name + '.get');
}
globalThis.fetch = async (input) => {
  const raw = typeof input === 'string' ? input : input?.url ?? String(input);
  let protocol = null, origin = null, pathname = null;
  try { const url = new URL(raw); protocol = url.protocol; if (protocol !== 'data:') { origin = url.origin; pathname = url.pathname; } } catch {}
  const event = {type: 'fetch', protocol, origin, pathname, bytes: Buffer.byteLength(raw),
    sha256: createHash('sha256').update(raw).digest('hex'), stack: new Error('blocked fetch diagnostic').stack};
  // This exact installed Yoga payload was independently diagnosed. Keep it
  // blocked: Yoga catches the failure and decodes its embedded bytes locally.
  // No original fetch is invoked for data, file, HTTP, or any other scheme.
  if (protocol === 'data:' && event.bytes === 95685 &&
      event.sha256 === '2a3af1723c9ab5dd5a0bf6ac4440cee82a86d74bb1de1b5ed95c75ef7ce05f87') {
    blockedDependencyDataFetches.push(event);
  } else {
    attempts.push(event);
  }
  throw new Error('offline fixture rejected fetch');
};
syncBuiltinESMExports();
process.once('exit', () => writeFileSync(process.env.FIXTURE_NETWORK_EVENTS,
  JSON.stringify({networkAttempts: attempts, blockedDependencyDataFetches}) + '\n', {flag: 'wx'}));
process.argv = [process.execPath, source, ...fixture.argv];
await import(pathToFileURL(source));

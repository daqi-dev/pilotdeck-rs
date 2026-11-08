// Run the actual CLI entry. Fail before any socket/fetch can leave this child.
import {readFileSync, writeFileSync} from 'node:fs';
import {createRequire, syncBuiltinESMExports} from 'node:module';
import {pathToFileURL} from 'node:url';
const [caseFile, source] = process.argv.slice(2);
const fixture = JSON.parse(readFileSync(caseFile, 'utf8'));
const attempts = [];
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
globalThis.fetch = async () => { attempts.push('fetch'); throw new Error('offline fixture rejected fetch'); };
syncBuiltinESMExports();
process.once('exit', () => writeFileSync(process.env.FIXTURE_NETWORK_EVENTS,
  JSON.stringify(attempts) + '\n', {flag: 'wx'}));
process.argv = [process.execPath, source, ...fixture.argv];
await import(pathToFileURL(source));

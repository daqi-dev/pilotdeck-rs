// Scripted stdio MCP server shared by the TS generator and the Rust replay.
// Plain Node ESM with no dependencies, so the Rust test can run it with any
// Node on PATH.
//
// MOCK_DIR holds `script.json` and receives one log per spawned process:
// each process claims the next index with an exclusive create, then appends
// JSON lines to `log-<index>.jsonl`:
//   {"event":"spawn","argv":[...],"cwd":"<rel>","env":{...}}
//   {"event":"frame","line":"<exact bytes received, without the newline>"}
//   {"event":"stdin-end"} / {"event":"exit"}
// `env` lists only variables outside the SDK's default inherited set, so it
// shows exactly what the spec contributed and that nothing else leaked.
//
// script.json: {"procs":[{"<method or tools/call:<name>>":[step...]}]}; process
// i uses procs[min(i, last)]. Steps run in order per received request:
//   {"send": value}   JSON.stringify(value) with every "$id" string -> request id
//   {"line": "text"}  raw line, "$ID" replaced with JSON.stringify(id)
//   {"sleep": ms}     wait
//   {"exit": code}    process.exit(code)
// A request without a handler is left unanswered (hang).
import fs from 'node:fs';
import path from 'node:path';

const dir = process.env.MOCK_DIR;
const script = JSON.parse(fs.readFileSync(path.join(dir, 'script.json'), 'utf8'));
let index = 0;
for (;; index += 1) {
  try {
    fs.closeSync(fs.openSync(path.join(dir, `proc-${index}.claim`), 'wx'));
    break;
  } catch (error) {
    if (error.code !== 'EEXIST') throw error;
  }
}
const logPath = path.join(dir, `log-${index}.jsonl`);
const log = (entry) => {
  try {
    fs.appendFileSync(logPath, JSON.stringify(entry) + '\n');
  } catch {
    // The run directory is gone: an orphan outlived its recorder.
  }
};
const DEFAULT_ENV = new Set(['HOME', 'LOGNAME', 'PATH', 'SHELL', 'TERM', 'USER']);
const env = {};
for (const key of Object.keys(process.env).sort()) {
  // macOS injects __CF_USER_TEXT_ENCODING into every process; it is not part
  // of what the client passed and does not exist on Linux.
  if (!DEFAULT_ENV.has(key) && key !== 'MOCK_DIR' && key !== '__CF_USER_TEXT_ENCODING') {
    env[key] = process.env[key];
  }
}
const cwd = path.relative(fs.realpathSync(dir), process.cwd());
log({
  event: 'spawn',
  argv: process.argv.slice(2),
  // An inherited cwd is the caller's and differs between the TS and Rust runs.
  cwd: cwd === '..' || cwd.startsWith('..' + path.sep) || path.isAbsolute(cwd) ? '<outside>' : cwd,
  env,
});
process.on('exit', () => log({ event: 'exit' }));
const procs = script.procs;
const handlers = procs[Math.min(index, procs.length - 1)];

function substitute(value, id) {
  if (value === '$id') return id;
  if (Array.isArray(value)) return value.map((item) => substitute(item, id));
  if (value && typeof value === 'object') {
    const out = {};
    for (const key of Object.keys(value)) out[key] = substitute(value[key], id);
    return out;
  }
  return value;
}

let queue = Promise.resolve();
// Consecutive output steps go out in one write, so the client sees them as
// one stdout chunk on every run (chunking decides JS microtask ordering).
async function run(steps, id) {
  let pending = '';
  const flush = () => {
    if (pending) process.stdout.write(pending);
    pending = '';
  };
  for (const step of steps) {
    if ('send' in step) pending += JSON.stringify(substitute(step.send, id)) + '\n';
    else if ('line' in step) pending += step.line.split('$ID').join(JSON.stringify(id)) + '\n';
    else if ('sleep' in step) {
      flush();
      await new Promise((resolve) => setTimeout(resolve, step.sleep));
    } else if ('exit' in step) {
      flush();
      process.exit(step.exit);
    }
  }
  flush();
}

let buffer = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  buffer += chunk;
  for (let at = buffer.indexOf('\n'); at !== -1; at = buffer.indexOf('\n')) {
    const line = buffer.slice(0, at);
    buffer = buffer.slice(at + 1);
    log({ event: 'frame', line });
    let message;
    try {
      message = JSON.parse(line);
    } catch {
      continue;
    }
    const key = message.method === 'tools/call' ? `tools/call:${message.params?.name}` : message.method;
    const steps = key === undefined ? undefined : handlers[key];
    if (steps) queue = queue.then(() => run(steps, message.id));
  }
});
process.stdin.on('end', () => {
  log({ event: 'stdin-end' });
  queue.then(() => process.exit(0));
});

// #552: the real TS NodeShellCommandRunner, run where its parent owns a
// controlling terminal (generate.mts wraps this file in `script -q
// /dev/null`, like a gateway started from an interactive terminal). The
// runner spawns its bash child `detached`, so the probe records what that
// child reports about its own session, process group and controlling
// terminal. Results go to argv[2] (a file): script's pty would CRLF-mangle
// anything printed to stdout.
import { openSync } from 'node:fs';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

if (!process.env.PDRS_BASELINE) throw Error('set PDRS_BASELINE');
const outFile = resolve(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);

// The parent's own /dev/tty, proving the recording ran with a controlling
// terminal (so the child's ENXIO is shown to hold despite one).
let parentDevTty = 'open';
try { openSync('/dev/tty', 'r+'); } catch (error) { parentDevTty = error.code; }

// What the spawned shell itself reports. `$$` is the shell's pid; each
// python3 probe is its direct child, so the comparisons are about the shell.
// ps(1) is only used for the controlling-terminal name (its `sess=` field is
// not the POSIX session id on macOS); the session and process group come from
// getsid(2)/getpgrp(2).
const commands = [
  ['session',
   'if /usr/bin/python3 -c "import os,sys; sys.exit(0 if os.getsid(0)==int(sys.argv[1]) else 1)" $$; then echo session=own; else echo session=inherited; fi'],
  ['pgroup',
   'if /usr/bin/python3 -c "import os,sys; sys.exit(0 if os.getpgrp()==int(sys.argv[1]) else 1)" $$; then echo pgroup=own; else echo pgroup=inherited; fi'],
  ['ctty',
   "t=$(ps -o tty= -p $$ | tr -d ' '); if [ \"$t\" = \"??\" ]; then echo ctty=none; else echo ctty=present; fi"],
  ['devtty',
   "/usr/bin/python3 -c \"import os\ntry:\n    os.open('/dev/tty', os.O_RDWR)\n    print('devtty=open')\nexcept OSError as e:\n    print('devtty=errno%d' % e.errno)\""],
];

const { NodeShellCommandRunner } = await import(
  pathToFileURL(join(baseline, 'src/tool/builtin/bash/commandRunner.ts')).href
);
const runner = new NodeShellCommandRunner();
const scratch = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-bash-session-'));
const cases = [];
try {
  for (const [name, command] of commands) {
    const result = await runner.run(command, {
      cwd: scratch,
      env: { PATH: '/usr/bin:/bin' },
      timeoutMs: 60_000,
    });
    // durationMs is wall clock; everything recorded must be reproducible.
    cases.push({ name, command, exitCode: result.exitCode, stdout: result.stdout, stderr: result.stderr, timedOut: result.timedOut });
  }
} finally {
  await rm(scratch, { recursive: true, force: true });
}
await writeFile(outFile, `${JSON.stringify({ parentDevTty, cases }, null, 2)}\n`);

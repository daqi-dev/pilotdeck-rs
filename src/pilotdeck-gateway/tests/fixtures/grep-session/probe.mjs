// #552 修复轮 1: the real TS spawn that `filesystem/ripgrep.ts` drives —
// `SubprocessPort.executeFile` (`createNodeSubprocessPort()`), whose
// `runNodeExecutable` spawns `detached: process.platform !== "win32"`.
// Run where the spawning parent owns a controlling terminal (generate.mts
// wraps this file in `script -q /dev/null`, like a gateway started from an
// interactive terminal). Each probe executable reports what the spawned
// process itself says about its own session, process group and controlling
// terminal. Results go to argv[2] (a file): script's pty would CRLF-mangle
// anything printed to stdout.
import { openSync } from 'node:fs';
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

if (!process.env.PDRS_BASELINE) throw Error('set PDRS_BASELINE');
const outFile = resolve(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);

// The spawning parent's own /dev/tty, proving the recording ran with a
// controlling terminal (so the child's ENXIO is shown to hold despite one).
let parentDevTty = 'open';
try { openSync('/dev/tty', 'r+'); } catch (error) { parentDevTty = error.code; }

// What the spawned executable itself reports. `$$` is the shell running the
// script — the process executeFile spawned; each python3 probe is its direct
// child, so the comparisons are about that shell. ps(1) is only used for the
// controlling-terminal name (its `sess=` field is not the POSIX session id on
// macOS); the session and process group come from getsid(2)/getpgrp(2).
const scripts = [
  ['session',
   '#!/bin/sh\nif /usr/bin/python3 -c "import os,sys; sys.exit(0 if os.getsid(0)==int(sys.argv[1]) else 1)" $$; then echo session=own; else echo session=inherited; fi\n'],
  ['pgroup',
   '#!/bin/sh\nif /usr/bin/python3 -c "import os,sys; sys.exit(0 if os.getpgrp()==int(sys.argv[1]) else 1)" $$; then echo pgroup=own; else echo pgroup=inherited; fi\n'],
  ['ctty',
   "#!/bin/sh\nt=$(ps -o tty= -p $$ | tr -d ' '); if [ \"$t\" = \"??\" ]; then echo ctty=none; else echo ctty=present; fi\n"],
  ['devtty',
   "#!/bin/sh\n/usr/bin/python3 -c \"import os\ntry:\n    os.open('/dev/tty', os.O_RDWR)\n    print('devtty=open')\nexcept OSError as e:\n    print('devtty=errno%d' % e.errno)\"\n"],
];

const { createNodeSubprocessPort } = await import(
  pathToFileURL(join(baseline, 'src/tool/execution-world/SubprocessPort.ts')).href
);
const subprocess = createNodeSubprocessPort();
if (!subprocess.executeFile) throw Error('subprocess port without executeFile');
const scratch = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-grep-session-'));
const cases = [];
try {
  for (const [name, script] of scripts) {
    const executable = join(scratch, `${name}.sh`);
    await writeFile(executable, script);
    await chmod(executable, 0o755);
    const result = await subprocess.executeFile({
      executable,
      args: [],
      cwd: scratch,
      env: { PATH: '/usr/bin:/bin' },
      timeoutMs: 60_000,
    });
    // durationMs is wall clock; everything recorded must be reproducible.
    cases.push({
      name,
      script,
      exitCode: result.exitCode,
      stdout: result.stdout,
      stderr: result.stderr,
      timedOut: result.timedOut,
    });
  }
} finally {
  await rm(scratch, { recursive: true, force: true });
}
await writeFile(outFile, `${JSON.stringify({ parentDevTty, cases }, null, 2)}\n`);

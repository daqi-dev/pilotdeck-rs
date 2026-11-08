#!/usr/bin/env python3
"""Owned-process signal restoration while a paste is incomplete; mbp2 only."""
import argparse, fcntl, hashlib, itertools, json, os, pty, select, signal, struct, subprocess, termios, time
from pathlib import Path
p = argparse.ArgumentParser()
for flag in ('bin', 'node', 'ts-root', 'out'):
    p.add_argument('--' + flag, required=True, type=Path)
a = p.parse_args()
out = a.out.resolve(); out.mkdir()
summary = []
# A shared open-file description and all distinct stdin/stdout NONBLOCK states.
layouts = [(False, False, False), (False, True, True)] + [(True, i, o) for i, o in itertools.product((False, True), repeat=2)]
for name, (split, input_nonblock, output_nonblock) in itertools.product(('SIGTERM', 'SIGINT', 'SIGHUP'), layouts):
    case = f'{name}-{"split" if split else "shared"}-{int(input_nonblock)}{int(output_nonblock)}'
    root = out / case; root.mkdir()
    home = root / 'home'; home.mkdir()
    project = root / 'project'; project.mkdir()
    wire = root / 'wire.jsonl'; wire.touch()
    env = {'PATH': '/usr/bin:/bin', 'HOME': str(home), 'PILOT_HOME': str(home), 'TERM': 'xterm-256color', 'LANG': 'en_US.UTF-8', 'NO_COLOR': '1'}
    children = []; master = slave = output_fd = None; raw = bytearray(); queries = 0; failure = None; observed = {}
    server = subprocess.Popen([str(a.node), str(Path(__file__).with_name('server.mjs')), str(a.ts_root), str(wire)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
    children.append(server)
    try:
        assert select.select([server.stdout], [], [], 8)[0]
        port = int(server.stdout.readline()); assert 0 < port < 65536 and port != 18789
        (home / 'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models: {{mock: {{}}}}\ngateway: {{port: {port}}}\n')
        (home / 'server-token').write_text('fixture-token\n')
        master, slave = pty.openpty(); fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 100, 0, 0))
        output_fd = os.open(os.ttyname(slave), os.O_RDWR | os.O_NOCTTY) if split else slave
        for fd, nonblock in ((slave, input_nonblock), (output_fd, output_nonblock)):
            value = fcntl.fcntl(fd, fcntl.F_GETFL)
            fcntl.fcntl(fd, fcntl.F_SETFL, value | os.O_NONBLOCK if nonblock else value & ~os.O_NONBLOCK)
        before = termios.tcgetattr(slave)
        flags_before = [fcntl.fcntl(fd, fcntl.F_GETFL) for fd in (slave, output_fd)]
        child = subprocess.Popen([str(a.bin), 'tui'], cwd=project, env=env, stdin=slave, stdout=output_fd, stderr=output_fd, start_new_session=True)
        children.append(child)
        def drain():
            global queries
            if select.select([master], [], [], .03)[0]:
                try: raw.extend(os.read(master, 65536))
                except OSError: pass
            while queries < bytes(raw).count(b'\x1b[6n'):
                os.write(master, b'\x1b[1;1R'); queries += 1
        until = time.monotonic() + 8
        while b'\x1b[?2004h' not in raw and time.monotonic() < until:
            drain(); assert child.poll() is None
        assert b'\x1b[?2004h' in raw, 'bracketed input was not enabled'
        assert termios.tcgetattr(slave) != before, 'raw mode never entered'
        os.write(master, b'\x1b[200~pending paste')
        until = time.monotonic() + .3
        while time.monotonic() < until: drain()
        child.send_signal(getattr(signal, name))
        until = time.monotonic() + 4
        while child.poll() is None and time.monotonic() < until: drain()
        for _ in range(5): drain()
        flags_after = [fcntl.fcntl(fd, fcntl.F_GETFL) for fd in (slave, output_fd)]
        # Darwin adds a kernel PTY status bit after ordinary IO. Compare the
        # NONBLOCK bit this runner changes; retain full raw values for audit.
        observed = {'exit': child.poll(), 'termios_restored': before == termios.tcgetattr(slave), 'flags_before': flags_before, 'flags_after': flags_after, 'nonblock_restored': all((a & os.O_NONBLOCK) == (b & os.O_NONBLOCK) for a, b in zip(flags_before, flags_after)), 'bracketed_disabled': b'\x1b[?2004l' in raw}
        assert observed['exit'] == -getattr(signal, name), observed
        assert all(observed[k] for k in ('termios_restored', 'nonblock_restored', 'bracketed_disabled')), observed
    except BaseException as error:
        failure = repr(error)
    finally:
        for child in reversed(children):
            if child.poll() is None: child.terminate()
            try: child.wait(timeout=3)
            except subprocess.TimeoutExpired: child.kill(); child.wait(timeout=3)
        if output_fd is not None and output_fd != slave: os.close(output_fd)
        if master is not None: os.close(master); os.close(slave)
        (root / 'terminal.bin').write_bytes(raw)
        (root / 'server.stderr').write_bytes(server.stderr.read())
        row = {'case': case, 'signal': name, 'split': split, 'failure': failure, **observed, 'children': [(c.pid, c.returncode) for c in children]}
        (root / 'result.json').write_text(json.dumps(row, indent=2) + '\n'); summary.append(row)
        print(json.dumps(row), flush=True)
(out / 'summary.json').write_text(json.dumps({'binary_sha256': hashlib.sha256(a.bin.read_bytes()).hexdigest(), 'cases': summary}, indent=2) + '\n')
raise SystemExit(0 if all(row['failure'] is None for row in summary) else 1)
